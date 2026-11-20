//! Plans as rows of a plan file's tables (schema version 2, in
//! `migrations/01-plans` and `migrations/02-imports`).
//!
//! Each kind of object is a [`Record`]: a row of its own table with an `id`,
//! a `position` giving its list order, and a column for each field. Saving a
//! change writes only the rows that differ from what the file holds.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use std::collections::{BTreeSet, HashMap};

use rusqlite::{Connection, Row, params};
use serde::Deserialize;
use serde::de::IntoDeserializer;

use super::Failure;
use crate::calendar::{self, Month};
use crate::import::{ImportOptions, StatementKey};
use crate::model::{
    Account, AccountType, Assigned, BankData, Category, Cleared, FlagColor, Group, Import, Payee,
    Plan, PlanInfo, READY_TO_ASSIGN, Subtransaction, Table, Transaction,
};
use crate::ofx::StatementType;

/// Stores serde enums by their wire names, such as `creditCard`, so a plan
/// file reads like the API.
macro_rules! wire_name_columns {
    ($($t:ty),*) => {$(
        impl ToSql for $t {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                match serde_json::to_value(self) {
                    Ok(serde_json::Value::String(name)) => Ok(name.into()),
                    _ => unreachable!("a unit variant serializes to its name"),
                }
            }
        }

        impl FromSql for $t {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                Self::deserialize(value.as_str()?.into_deserializer())
                    .map_err(|e: serde::de::value::Error| FromSqlError::Other(e.into()))
            }
        }
    )*};
}

wire_name_columns!(AccountType, Cleared, FlagColor, StatementType);

impl ToSql for Month {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}

impl FromSql for Month {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|_| FromSqlError::Other("not a month YYYY-MM".into()))
    }
}

/// An object kept as a row of its own table, which has an `id` primary key
/// and a unique `position`: listing the rows by `position` lists the objects
/// in their order.
trait Record: PartialEq + Sized {
    const TABLE: &str;

    fn id(&self) -> &str;

    /// The row's other columns, and their values.
    fn columns(&self) -> Vec<(&'static str, &dyn ToSql)>;

    fn from_row(row: &Row) -> rusqlite::Result<Self>;

    /// Writes the rows kept in other tables as part of this object, such as
    /// a transaction's subtransactions, replacing any there were.
    fn write_parts(&self, _conn: &Connection) -> rusqlite::Result<()> {
        Ok(())
    }

    /// Deletes the rows kept in other tables as part of the object `id`.
    fn delete_parts(_conn: &Connection, _id: &str) -> rusqlite::Result<()> {
        Ok(())
    }
}

impl Record for Account {
    const TABLE: &str = "accounts";

    fn id(&self) -> &str {
        &self.id
    }

    fn columns(&self) -> Vec<(&'static str, &dyn ToSql)> {
        vec![
            ("name", &self.name),
            ("type", &self.kind),
            ("closed", &self.closed),
            ("note", &self.note),
            ("transfer_payee_id", &self.transfer_payee_id),
            ("payment_category_id", &self.payment_category_id),
        ]
    }

    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            kind: row.get("type")?,
            closed: row.get("closed")?,
            note: row.get("note")?,
            transfer_payee_id: row.get("transfer_payee_id")?,
            payment_category_id: row.get("payment_category_id")?,
        })
    }
}

impl Record for Payee {
    const TABLE: &str = "payees";

    fn id(&self) -> &str {
        &self.id
    }

    fn columns(&self) -> Vec<(&'static str, &dyn ToSql)> {
        vec![
            ("name", &self.name),
            ("transfer_account_id", &self.transfer_account_id),
        ]
    }

    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            transfer_account_id: row.get("transfer_account_id")?,
        })
    }
}

impl Record for Group {
    const TABLE: &str = "category_groups";

    fn id(&self) -> &str {
        &self.id
    }

    fn columns(&self) -> Vec<(&'static str, &dyn ToSql)> {
        vec![
            ("name", &self.name),
            ("hidden", &self.hidden),
            ("internal", &self.internal),
        ]
    }

    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            hidden: row.get("hidden")?,
            internal: row.get("internal")?,
        })
    }
}

impl Record for Category {
    const TABLE: &str = "categories";

    fn id(&self) -> &str {
        &self.id
    }

    fn columns(&self) -> Vec<(&'static str, &dyn ToSql)> {
        vec![
            ("group_id", &self.group_id),
            ("name", &self.name),
            ("hidden", &self.hidden),
            ("note", &self.note),
            ("payment_account_id", &self.payment_account_id),
        ]
    }

    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            group_id: row.get("group_id")?,
            name: row.get("name")?,
            hidden: row.get("hidden")?,
            note: row.get("note")?,
            payment_account_id: row.get("payment_account_id")?,
        })
    }
}

impl Record for Transaction {
    const TABLE: &str = "transactions";

    fn id(&self) -> &str {
        &self.id
    }

    fn columns(&self) -> Vec<(&'static str, &dyn ToSql)> {
        vec![
            ("account_id", &self.account_id),
            ("date", &self.date),
            ("amount", &self.amount),
            ("payee_id", &self.payee_id),
            ("category_id", &self.category_id),
            ("memo", &self.memo),
            ("cleared", &self.cleared),
            ("approved", &self.approved),
            ("flag_color", &self.flag_color),
            ("transfer_account_id", &self.transfer_account_id),
            ("transfer_transaction_id", &self.transfer_transaction_id),
        ]
    }

    /// The transaction without its subtransactions and its import, which
    /// [`read_plan`] reads from their own tables.
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            account_id: row.get("account_id")?,
            date: row.get("date")?,
            amount: row.get("amount")?,
            payee_id: row.get("payee_id")?,
            category_id: row.get("category_id")?,
            memo: row.get("memo")?,
            cleared: row.get("cleared")?,
            approved: row.get("approved")?,
            flag_color: row.get("flag_color")?,
            transfer_account_id: row.get("transfer_account_id")?,
            transfer_transaction_id: row.get("transfer_transaction_id")?,
            subtransactions: Vec::new(),
            import: None,
        })
    }

    fn write_parts(&self, conn: &Connection) -> rusqlite::Result<()> {
        Self::delete_parts(conn, &self.id)?;
        let mut insert = conn.prepare_cached(
            "INSERT INTO subtransactions (transaction_id, position, amount, category_id, memo) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (position, s) in (1_i64..).zip(&self.subtransactions) {
            insert.execute(params![self.id, position, s.amount, s.category_id, s.memo])?;
        }
        if let Some(Import { data, matched }) = &self.import {
            conn.prepare_cached(
                "INSERT INTO transaction_imports \
                 (transaction_id, import_id, date, amount, payee_name, memo, matched) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![
                self.id,
                data.id,
                data.date,
                data.amount,
                data.payee_name,
                data.memo,
                matched
            ])?;
        }
        Ok(())
    }

    fn delete_parts(conn: &Connection, id: &str) -> rusqlite::Result<()> {
        conn.prepare_cached("DELETE FROM subtransactions WHERE transaction_id = ?1")?
            .execute([id])?;
        conn.prepare_cached("DELETE FROM transaction_imports WHERE transaction_id = ?1")?
            .execute([id])?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Writing

/// Writes the rows of `new` that differ from `old`, which is what the table
/// holds.
///
/// Rows keep their `position` while they stay in the order they were in.
/// From the first row that's new, or that now comes before a row it used to
/// follow, every row is written at the end, in order. That's where a `Table`
/// puts new objects, and categories that move to another group (SPEC 7.6), so
/// usually only those rows get a new `position`.
fn write_table<R: Record>(
    conn: &Connection,
    old: &Table<R>,
    new: &Table<R>,
) -> rusqlite::Result<()> {
    for id in old.keys().filter(|id| !new.contains_key(*id)) {
        conn.prepare_cached(&format!("DELETE FROM {} WHERE id = ?1", R::TABLE))?
            .execute([id])?;
        R::delete_parts(conn, id)?;
    }
    let mut last_kept = None;
    let mut next_position = None;
    for row in new.values() {
        match old.get_full(row.id()) {
            Some((index, _, before)) if next_position.is_none() && last_kept < Some(index) => {
                last_kept = Some(index);
                if before != row {
                    update(conn, row)?;
                }
            }
            _ => {
                let position = match next_position {
                    Some(position) => position,
                    None => conn.query_row(
                        &format!("SELECT IFNULL(MAX(position), 0) + 1 FROM {}", R::TABLE),
                        [],
                        |r| r.get(0),
                    )?,
                };
                put_at(conn, row, position)?;
                next_position = Some(position + 1);
            }
        }
    }
    Ok(())
}

/// Writes a row's columns in place, keeping its position.
fn update<R: Record>(conn: &Connection, row: &R) -> rusqlite::Result<()> {
    let columns = row.columns();
    let set: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(i, (name, _))| format!("{name} = ?{}", i + 2))
        .collect();
    let sql = format!("UPDATE {} SET {} WHERE id = ?1", R::TABLE, set.join(", "));
    let id = row.id();
    let values = std::iter::once(&id as &dyn ToSql).chain(columns.iter().map(|(_, v)| *v));
    conn.prepare_cached(&sql)?
        .execute(rusqlite::params_from_iter(values))?;
    row.write_parts(conn)
}

/// Writes a row at `position`, adding it if it's new.
fn put_at<R: Record>(conn: &Connection, row: &R, position: i64) -> rusqlite::Result<()> {
    let columns = row.columns();
    let names: Vec<&str> = columns.iter().map(|(name, _)| *name).collect();
    let placeholders: Vec<String> = (3..names.len() + 3).map(|i| format!("?{i}")).collect();
    let updates: Vec<String> = names
        .iter()
        .map(|name| format!("{name} = excluded.{name}"))
        .collect();
    let sql = format!(
        "INSERT INTO {} (id, position, {}) VALUES (?1, ?2, {}) \
         ON CONFLICT (id) DO UPDATE SET position = excluded.position, {}",
        R::TABLE,
        names.join(", "),
        placeholders.join(", "),
        updates.join(", "),
    );
    let id = row.id();
    let values = [&id as &dyn ToSql, &position]
        .into_iter()
        .chain(columns.iter().map(|(_, v)| *v));
    conn.prepare_cached(&sql)?
        .execute(rusqlite::params_from_iter(values))?;
    row.write_parts(conn)
}

fn write_assigned(conn: &Connection, old: &Assigned, new: &Assigned) -> rusqlite::Result<()> {
    let amount = |assigned: &Assigned, category: &str, month: &Month| {
        assigned.get(category)?.get(month).copied()
    };
    for (category, months) in old {
        for month in months.keys().filter(|m| amount(new, category, m).is_none()) {
            conn.prepare_cached("DELETE FROM assignments WHERE category_id = ?1 AND month = ?2")?
                .execute(params![category, month])?;
        }
    }
    for (category, months) in new {
        for (month, &assigned) in months
            .iter()
            .filter(|&(m, &a)| amount(old, category, m) != Some(a))
        {
            conn.prepare_cached(
                "INSERT INTO assignments (category_id, month, assigned) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (category_id, month) DO UPDATE SET assigned = excluded.assigned",
            )?
            .execute(params![category, month, assigned])?;
        }
    }
    Ok(())
}

/// The import ids each account has had (SPEC 17.9).
type ImportIds = HashMap<String, BTreeSet<String>>;

fn write_import_ids(conn: &Connection, old: &ImportIds, new: &ImportIds) -> rusqlite::Result<()> {
    let pairs = |ids: &ImportIds| -> BTreeSet<(String, String)> {
        ids.iter()
            .flat_map(|(account, ids)| ids.iter().map(|id| (account.clone(), id.clone())))
            .collect()
    };
    let (old, new) = (pairs(old), pairs(new));
    for (account, id) in old.difference(&new) {
        conn.prepare_cached(
            "DELETE FROM account_import_ids WHERE account_id = ?1 AND import_id = ?2",
        )?
        .execute([account, id])?;
    }
    for (account, id) in new.difference(&old) {
        conn.prepare_cached(
            "INSERT INTO account_import_ids (account_id, import_id) VALUES (?1, ?2)",
        )?
        .execute([account, id])?;
    }
    Ok(())
}

/// What imports remember, by statement (SPEC 17.13).
type RememberedOptions = HashMap<StatementKey, ImportOptions>;

fn write_import_options(
    conn: &Connection,
    old: &RememberedOptions,
    new: &RememberedOptions,
) -> rusqlite::Result<()> {
    for (key, options) in old {
        if new.get(key) != Some(options) {
            conn.prepare_cached(
                "DELETE FROM import_options \
                 WHERE type = ?1 AND bank_id IS ?2 AND account_number = ?3",
            )?
            .execute(params![key.kind, key.bank_id, key.account_number])?;
        }
    }
    for (key, options) in new {
        if old.get(key) != Some(options) {
            conn.prepare_cached(
                "INSERT INTO import_options (type, bank_id, account_number, account_id, \
                 swap_payee_memo, invert_amounts) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                key.kind,
                key.bank_id,
                key.account_number,
                options.account_id,
                options.swap_payee_memo,
                options.invert_amounts
            ])?;
        }
    }
    Ok(())
}

/// Writes the changes from `old`, the plan the file holds, to `new`. With no
/// `old`, the file is new and holds no plan yet.
pub fn write_plan(conn: &Connection, old: Option<&Plan>, new: &Plan) -> rusqlite::Result<()> {
    let info = &new.info;
    match old {
        None => {
            conn.execute(
                "INSERT INTO plan (id, name, currency_code, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![info.id, info.name, info.currency_code, new.created_at],
            )?;
        }
        Some(old) if old.info != new.info => {
            conn.execute(
                "UPDATE plan SET name = ?1, currency_code = ?2",
                params![info.name, info.currency_code],
            )?;
        }
        Some(_) => {}
    }
    write_table(
        conn,
        old.map_or(&Table::new(), |p| &p.accounts),
        &new.accounts,
    )?;
    write_table(conn, old.map_or(&Table::new(), |p| &p.payees), &new.payees)?;
    write_table(conn, old.map_or(&Table::new(), |p| &p.groups), &new.groups)?;
    write_table(
        conn,
        old.map_or(&Table::new(), |p| &p.categories),
        &new.categories,
    )?;
    write_table(
        conn,
        old.map_or(&Table::new(), |p| &p.transactions),
        &new.transactions,
    )?;
    write_assigned(
        conn,
        old.map_or(&Assigned::new(), |p| &p.assigned),
        &new.assigned,
    )?;
    write_import_ids(
        conn,
        old.map_or(&ImportIds::new(), |p| &p.import_ids),
        &new.import_ids,
    )?;
    write_import_options(
        conn,
        old.map_or(&RememberedOptions::new(), |p| &p.import_options),
        &new.import_options,
    )
}

// ---------------------------------------------------------------------------
// Reading

fn read_table<R: Record>(conn: &Connection) -> rusqlite::Result<Table<R>> {
    conn.prepare(&format!("SELECT * FROM {} ORDER BY position", R::TABLE))?
        .query_map([], R::from_row)?
        .map(|row| row.map(|r| (r.id().to_owned(), r)))
        .collect()
}

/// Reads the plan a file holds, checking that it's one the server can serve.
pub fn read_plan(conn: &Connection) -> Result<Plan, Failure> {
    let mut plan_rows = conn.prepare("SELECT id, name, currency_code, created_at FROM plan")?;
    let plan_rows: Vec<(PlanInfo, i64)> = plan_rows
        .query_map([], |r| {
            Ok((
                PlanInfo {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    currency_code: r.get(2)?,
                },
                r.get(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let [(info, created_at)] =
        <[_; 1]>::try_from(plan_rows).map_err(|_| "the plan table must have one row")?;

    let mut transactions: Table<Transaction> = read_table(conn)?;
    let mut subtransactions =
        conn.prepare("SELECT * FROM subtransactions ORDER BY transaction_id, position")?;
    let subtransactions = subtransactions.query_map([], |r| {
        let s = Subtransaction {
            amount: r.get("amount")?,
            category_id: r.get("category_id")?,
            memo: r.get("memo")?,
        };
        Ok((r.get::<_, String>("transaction_id")?, s))
    })?;
    for row in subtransactions {
        let (transaction_id, s) = row?;
        let t = transactions
            .get_mut(&transaction_id)
            .ok_or("a subtransaction's transaction is missing")?;
        t.subtransactions.push(s);
    }

    let mut imports = conn.prepare("SELECT * FROM transaction_imports")?;
    let imports = imports.query_map([], |r| {
        let import = Import {
            data: BankData {
                id: r.get("import_id")?,
                date: r.get("date")?,
                amount: r.get("amount")?,
                payee_name: r.get("payee_name")?,
                memo: r.get("memo")?,
            },
            matched: r.get("matched")?,
        };
        Ok((r.get::<_, String>("transaction_id")?, import))
    })?;
    for row in imports {
        let (transaction_id, import) = row?;
        let t = transactions
            .get_mut(&transaction_id)
            .ok_or("an import's transaction is missing")?;
        t.import = Some(import);
    }

    let mut assigned = Assigned::new();
    let mut assignments = conn.prepare("SELECT category_id, month, assigned FROM assignments")?;
    for row in assignments.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
        let (category_id, month, amount): (String, Month, i64) = row?;
        assigned
            .entry(category_id)
            .or_default()
            .insert(month, amount);
    }

    let mut import_ids = ImportIds::new();
    let mut ids = conn.prepare("SELECT account_id, import_id FROM account_import_ids")?;
    for row in ids.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
        let (account_id, import_id): (String, String) = row?;
        import_ids.entry(account_id).or_default().insert(import_id);
    }

    let mut options = conn.prepare("SELECT * FROM import_options")?;
    let import_options = options
        .query_map([], |r| {
            let key = StatementKey {
                kind: r.get("type")?,
                bank_id: r.get("bank_id")?,
                account_number: r.get("account_number")?,
            };
            let options = ImportOptions {
                account_id: r.get("account_id")?,
                swap_payee_memo: r.get("swap_payee_memo")?,
                invert_amounts: r.get("invert_amounts")?,
            };
            Ok((key, options))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let plan = Plan {
        info,
        created_at,
        accounts: read_table(conn)?,
        payees: read_table(conn)?,
        groups: read_table(conn)?,
        categories: read_table(conn)?,
        transactions,
        assigned,
        import_ids,
        import_options,
    };
    check_links(&plan)?;
    Ok(plan)
}

/// Checks the links between a plan's objects that the server relies on, so
/// that a file edited by hand is skipped (SPEC 15.7) rather than served with
/// links that lead nowhere.
fn check_links(plan: &Plan) -> Result<(), String> {
    check_groups(plan)?;
    check_accounts(plan)?;
    check_payees(plan)?;
    check_categories(plan)?;
    check_transactions(plan)?;
    check_imports(plan)
}

/// `Ok` if `ok`, and otherwise the error naming `problem`.
fn ensure(ok: bool, problem: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(problem.to_owned()) }
}

fn check_groups(plan: &Plan) -> Result<(), String> {
    let mut groups = plan.groups.values();
    ensure(
        groups.next().is_some_and(|g| g.internal),
        "the first category group must be Credit Card Payments",
    )?;
    ensure(
        groups.all(|g| !g.internal),
        "only one category group may be internal",
    )
}

fn check_accounts(plan: &Plan) -> Result<(), String> {
    for a in plan.accounts.values() {
        let payee = plan.payees.get(&a.transfer_payee_id);
        ensure(
            payee.is_some_and(|p| p.transfer_account_id.as_ref() == Some(&a.id)),
            "an account's transfer payee is missing",
        )?;
        let payment = a
            .payment_category_id
            .as_ref()
            .map(|id| plan.categories.get(id));
        ensure(
            match payment {
                Some(c) => {
                    a.kind.is_credit()
                        && c.is_some_and(|c| c.payment_account_id.as_ref() == Some(&a.id))
                }
                None => !a.kind.is_credit(),
            },
            "a credit account's payment category is missing",
        )?;
    }
    Ok(())
}

fn check_payees(plan: &Plan) -> Result<(), String> {
    for p in plan.payees.values() {
        let account = p
            .transfer_account_id
            .as_ref()
            .map(|id| plan.accounts.get(id));
        ensure(
            account.is_none_or(|a| a.is_some_and(|a| a.transfer_payee_id == p.id)),
            "a transfer payee's account is missing",
        )?;
    }
    Ok(())
}

fn check_categories(plan: &Plan) -> Result<(), String> {
    for c in plan.categories.values() {
        let group = plan
            .groups
            .get(&c.group_id)
            .ok_or("a category's group is missing")?;
        ensure(
            group.internal == c.payment_account_id.is_some(),
            "Credit Card Payments must hold the payment categories, and only them",
        )?;
        let account = c
            .payment_account_id
            .as_ref()
            .map(|id| plan.accounts.get(id));
        ensure(
            account
                .is_none_or(|a| a.is_some_and(|a| a.payment_category_id.as_ref() == Some(&c.id))),
            "a payment category's account is missing",
        )?;
    }
    Ok(())
}

fn check_transactions(plan: &Plan) -> Result<(), String> {
    let category_exists = |id: &str| id == READY_TO_ASSIGN || plan.categories.contains_key(id);
    for t in plan.transactions.values() {
        ensure(
            plan.accounts.contains_key(&t.account_id),
            "a transaction's account is missing",
        )?;
        ensure(
            calendar::is_date(&t.date),
            "a transaction's date isn't a date",
        )?;
        ensure(
            t.payee_id
                .as_ref()
                .is_none_or(|id| plan.payees.contains_key(id)),
            "a transaction's payee is missing",
        )?;
        ensure(
            t.category_id.as_deref().is_none_or(category_exists),
            "a transaction's category is missing",
        )?;
        ensure(
            t.subtransactions
                .iter()
                .all(|s| category_exists(&s.category_id)),
            "a subtransaction's category is missing",
        )?;
        let other = t
            .transfer_transaction_id
            .as_ref()
            .map(|id| plan.transactions.get(id));
        ensure(
            match other {
                Some(o) => o.is_some_and(|o| {
                    o.transfer_transaction_id.as_ref() == Some(&t.id)
                        && t.transfer_account_id.as_ref() == Some(&o.account_id)
                }),
                None => t.transfer_account_id.is_none(),
            },
            "a transfer's other half is missing",
        )?;
    }
    Ok(())
}

fn check_imports(plan: &Plan) -> Result<(), String> {
    ensure(
        plan.transactions
            .values()
            .filter_map(|t| t.import.as_ref())
            .all(|i| calendar::is_date(&i.data.date)),
        "an import's date isn't a date",
    )?;
    ensure(
        plan.import_ids
            .keys()
            .chain(plan.import_options.values().map(|o| &o.account_id))
            .all(|id| plan.accounts.contains_key(id)),
        "an import's account is missing",
    )
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use super::*;
    use crate::model::new_id;

    fn assert_rows<T: PartialEq + Debug>(read: &Table<T>, written: &Table<T>) {
        // IndexMap's `==` ignores order.
        assert_eq!(
            read.iter().collect::<Vec<_>>(),
            written.iter().collect::<Vec<_>>()
        );
    }

    /// Reads the plan back and checks it's the one written, in every order.
    fn assert_holds(conn: &Connection, written: &Plan) {
        let read = read_plan(conn).unwrap();
        assert_eq!(
            (&read.info, read.created_at),
            (&written.info, written.created_at)
        );
        assert_rows(&read.accounts, &written.accounts);
        assert_rows(&read.payees, &written.payees);
        assert_rows(&read.groups, &written.groups);
        assert_rows(&read.categories, &written.categories);
        assert_rows(&read.transactions, &written.transactions);
        assert_eq!(read.assigned, written.assigned);
        assert_eq!(read.import_ids, written.import_ids);
        assert_eq!(read.import_options, written.import_options);
    }

    fn add_category(plan: &mut Plan, group_id: &str, name: &str) -> String {
        let id = new_id();
        let category = Category {
            id: id.clone(),
            group_id: group_id.to_owned(),
            name: name.to_owned(),
            hidden: false,
            note: None,
            payment_account_id: None,
        };
        plan.categories.insert(id.clone(), category);
        id
    }

    fn add_group(plan: &mut Plan, name: &str) -> String {
        let id = new_id();
        plan.groups.insert(
            id.clone(),
            Group {
                id: id.clone(),
                name: name.to_owned(),
                hidden: false,
                internal: false,
            },
        );
        id
    }

    fn add_transaction(
        plan: &mut Plan,
        account_id: &str,
        subtransactions: Vec<Subtransaction>,
    ) -> String {
        let id = new_id();
        let category_id = subtransactions
            .is_empty()
            .then(|| READY_TO_ASSIGN.to_owned());
        let amount = subtransactions.iter().map(|s| s.amount).sum();
        let t = Transaction {
            id: id.clone(),
            account_id: account_id.to_owned(),
            date: "2026-10-05".into(),
            amount,
            category_id,
            subtransactions,
            ..Default::default()
        };
        plan.transactions.insert(id.clone(), t);
        id
    }

    #[test]
    fn saves_write_every_change_in_order() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::super::migrations().to_latest(&mut conn).unwrap();
        let mut plan = Plan::new("Household".into(), "USD".into(), 42);
        let checking = plan.create_account(
            "Checking".into(),
            AccountType::Checking,
            None,
            Some((1000, "2026-10-01".into())),
        );
        plan.create_account(
            "Visa".into(),
            AccountType::CreditCard,
            Some("gold".into()),
            Some((-500, "2026-10-01".into())),
        );
        let bills = add_group(&mut plan, "Bills");
        let everyday = add_group(&mut plan, "Everyday");
        let [a, b, c, d] = ["A", "B", "C", "D"].map(|name| add_category(&mut plan, &bills, name));
        let line = |amount, category_id: &str, memo: Option<&str>| Subtransaction {
            amount,
            category_id: category_id.to_owned(),
            memo: memo.map(str::to_owned),
        };
        let early = add_transaction(&mut plan, &checking, vec![]);
        let split = add_transaction(
            &mut plan,
            &checking,
            vec![line(-3, &a, Some("x")), line(-4, &b, None)],
        );
        plan.assigned
            .entry(a.clone())
            .or_default()
            .insert("2026-10".parse().unwrap(), 7);
        write_plan(&conn, None, &plan).unwrap();
        assert_holds(&conn, &plan);

        let old = plan.clone();
        plan.info.name = "Renamed".into();
        // B then A move to the end of Everyday, after a new category; C goes.
        add_category(&mut plan, &everyday, "E");
        for moved in [&b, &a] {
            let mut category = plan.categories.shift_remove(moved).unwrap();
            category.group_id = everyday.clone();
            plan.categories.insert(moved.clone(), category);
        }
        plan.delete_category(&c, Some(&d));
        plan.transactions.shift_remove(&early);
        let edited = plan.transactions.get_mut(&split).unwrap();
        edited.subtransactions = vec![
            line(-1, &d, None),
            line(-2, &d, None),
            line(-4, &b, Some("y")),
        ];
        edited.amount = -7;
        add_transaction(&mut plan, &checking, vec![]);
        let unused = plan.payee_by_name("Unused");
        plan.assigned.remove(&a);
        plan.assigned
            .entry(d.clone())
            .or_default()
            .insert("2026-11".parse().unwrap(), -2);
        write_plan(&conn, Some(&old), &plan).unwrap();
        assert_holds(&conn, &plan);

        // Moving one back works from positions with gaps.
        let old = plan.clone();
        let mut category = plan.categories.shift_remove(&a).unwrap();
        category.group_id = bills;
        plan.categories.insert(a, category);
        plan.payees.shift_remove(&unused);
        write_plan(&conn, Some(&old), &plan).unwrap();
        assert_holds(&conn, &plan);
    }
}
