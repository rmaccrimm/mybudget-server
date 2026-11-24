//! The data the server keeps, and its JSON views.
//!
//! Every collection is a [`Table`]: an `IndexMap` from id to object, oldest
//! first. `shift_remove` keeps that order when something is deleted.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::calendar::Month;
use crate::error::ApiError;
use crate::import::{ImportOptions, StatementKey};

/// The category id of Inflow: Ready to Assign (SPEC 7.7). Generated ids
/// always contain a `-`, so none equals it.
pub const READY_TO_ASSIGN: &str = "ready_to_assign";
/// The payee of starting balance transactions (SPEC 5.3).
pub const STARTING_BALANCE_PAYEE: &str = "Starting Balance";
/// The payee of reconciliation adjustments (SPEC 14.2).
pub const RECONCILIATION_PAYEE: &str = "Reconciliation Balance Adjustment";
/// The internal group's name (SPEC 7.1).
pub const CREDIT_CARD_PAYMENTS: &str = "Credit Card Payments";

pub fn transfer_payee_name(account_name: &str) -> String {
    format!("Transfer : {account_name}")
}

/// A new id: a random version 4 UUID in lowercase with hyphens, so ids made
/// by different servers don't collide (SPEC 3.5).
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Objects by id, oldest first.
pub type Table<T> = IndexMap<String, T>;

/// Looking up an object named in the path, which is 404 if it doesn't exist
/// (SPEC 3.4).
pub trait ById<T> {
    fn by_id(&self, id: &str) -> Result<&T, ApiError>;
    fn by_id_mut(&mut self, id: &str) -> Result<&mut T, ApiError>;
    /// Looking up an object a body field names, which is 400 if it doesn't
    /// exist (SPEC 3.4).
    fn referenced(&self, field: &str, id: &str) -> Result<&T, ApiError>;
}

impl<T> ById<T> for Table<T> {
    fn by_id(&self, id: &str) -> Result<&T, ApiError> {
        self.get(id).ok_or_else(ApiError::not_found)
    }

    fn by_id_mut(&mut self, id: &str) -> Result<&mut T, ApiError> {
        self.get_mut(id).ok_or_else(ApiError::not_found)
    }

    fn referenced(&self, field: &str, id: &str) -> Result<&T, ApiError> {
        self.get(id).ok_or_else(|| {
            ApiError::bad_request(format!("{field} doesn't name anything in the plan"))
        })
    }
}

// ---------------------------------------------------------------------------
// Entities

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountType {
    Checking,
    Savings,
    Cash,
    CreditCard,
    LineOfCredit,
    OtherAsset,
    OtherLiability,
}

impl AccountType {
    pub fn is_credit(self) -> bool {
        matches!(self, Self::CreditCard | Self::LineOfCredit)
    }

    /// Checking, savings and cash accounts. Income counts only in these
    /// (SPEC 11.6); in a credit account `ready_to_assign` doesn't touch the
    /// plan (SPEC 12.6).
    pub fn is_cash(self) -> bool {
        matches!(self, Self::Checking | Self::Savings | Self::Cash)
    }

    pub fn on_budget(self) -> bool {
        !matches!(self, Self::OtherAsset | Self::OtherLiability)
    }
}

#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: AccountType,
    pub closed: bool,
    pub note: Option<String>,
    pub transfer_payee_id: String,
    pub payment_category_id: Option<String>,
}

/// An account's balances (SPEC 5.4).
#[derive(Default, Serialize)]
pub struct Balances {
    pub balance: i64,
    pub cleared_balance: i64,
    pub uncleared_balance: i64,
}

/// An account as the API shows it (SPEC 5.1).
#[derive(Serialize)]
pub struct AccountView {
    #[serde(flatten)]
    account: Account,
    on_budget: bool,
    #[serde(flatten)]
    balances: Balances,
}

#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Payee {
    pub id: String,
    pub name: String,
    pub transfer_account_id: Option<String>,
}

impl Payee {
    pub fn is_transfer(&self) -> bool {
        self.transfer_account_id.is_some()
    }
}

/// A category group. Its categories are the ones whose `group_id` names it.
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub hidden: bool,
    pub internal: bool,
}

/// A group as the API shows it, with its categories (SPEC 7.1).
#[derive(Serialize)]
pub struct GroupView {
    #[serde(flatten)]
    group: Group,
    categories: Vec<CategoryView>,
}

#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Category {
    pub id: String,
    pub group_id: String,
    pub name: String,
    pub hidden: bool,
    pub note: Option<String>,
    pub payment_account_id: Option<String>,
    /// Every target it has had and still keeps, by `start_month`, none of
    /// them applying to the same month (SPEC 18.4).
    #[serde(skip)]
    pub targets: Vec<Target>,
}

/// A category as the API shows it, with its current target (SPEC 18.3).
#[derive(Serialize)]
pub struct CategoryView {
    #[serde(flatten)]
    category: Category,
    target: Option<Target>,
}

impl Category {
    pub fn view(&self) -> CategoryView {
        CategoryView {
            category: self.clone(),
            target: self.current_target().cloned(),
        }
    }

    /// The target without end, if there is one (SPEC 18.4).
    pub fn current_target(&self) -> Option<&Target> {
        self.targets.last().filter(|t| t.end_month.is_none())
    }

    /// The target that applies in `month` (SPEC 18.4, 18.5).
    pub fn target_in(&self, month: Month) -> Option<&Target> {
        self.targets.iter().find(|t| t.applies_in(month))
    }

    /// Sets a new target from its `start_month` on (SPEC 18.4), unless the
    /// current target already asks for the same since then or earlier.
    pub fn set_target(&mut self, target: Target) {
        if self
            .current_target()
            .is_some_and(|t| t.goal == target.goal && t.start_month <= target.start_month)
        {
            return;
        }
        self.end_targets_at(target.start_month);
        self.targets.push(target);
    }

    /// Leaves no target in `month` or later: those that start then or later
    /// are removed, and the one that applies then ends the month before
    /// (SPEC 18.4).
    pub fn end_targets_at(&mut self, month: Month) {
        self.targets.retain(|t| t.start_month < month);
        if let Some(t) = self.targets.last_mut().filter(|t| t.applies_in(month)) {
            t.end_month = Some(month.prev());
        }
    }
}

/// How a target counts what a category needs (SPEC 18.3, 18.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetType {
    /// Assign `amount` every month.
    SetAside,
    /// Fill the category up to `amount` every month.
    Refill,
}

/// What a target asks for each month it applies to (SPEC 18.3). Setting a
/// target that asks for what the current one does changes nothing (18.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub struct Goal {
    #[serde(rename = "type")]
    pub kind: TargetType,
    /// Greater than 0.
    pub amount: i64,
    /// The day of the month it's due, from 1 to 31, or `None` for the
    /// month's last day.
    pub by_date: Option<u8>,
}

impl Goal {
    /// How much more to assign in a month, given what the category carried
    /// in (SPEC 11.4) and what's assigned (SPEC 18.5).
    pub fn underfunded(&self, carryover: i64, assigned: i64) -> i64 {
        let counted = match self.kind {
            TargetType::SetAside => assigned,
            TargetType::Refill => carryover + assigned,
        };
        (self.amount - counted).max(0)
    }
}

/// A category's target over the months it applies to (SPEC 18.4).
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Target {
    #[serde(flatten)]
    pub goal: Goal,
    pub start_month: Month,
    /// The last month it applies to, or `None` if it has no end. Only the
    /// API's four fields are shown (SPEC 18.3).
    #[serde(skip)]
    pub end_month: Option<Month>,
}

impl Target {
    fn applies_in(&self, month: Month) -> bool {
        self.start_month <= month && self.end_month.is_none_or(|end| month <= end)
    }
}

/// A change to a category's `assigned` for a month (SPEC 18.2). The API
/// shows only its `date` and `amount`.
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Assignment {
    /// Only for keeping it in a plan file.
    #[serde(skip)]
    pub id: String,
    #[serde(skip)]
    pub category_id: String,
    #[serde(skip)]
    pub month: Month,
    /// The date it was made.
    pub date: String,
    /// What it adds to `assigned`, positive or negative.
    pub amount: i64,
}

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cleared {
    #[default]
    Uncleared,
    Cleared,
    Reconciled,
}

impl Cleared {
    /// A new transaction's default: money in a wallet clears at once
    /// (SPEC 8.1).
    pub fn default_for(kind: AccountType) -> Self {
        if kind == AccountType::Cash {
            Self::Cleared
        } else {
            Self::Uncleared
        }
    }
}

/// Flag colours (SPEC 8.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlagColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Subtransaction {
    pub amount: i64,
    pub category_id: String,
    pub memo: Option<String>,
}

#[derive(Clone, PartialEq, Debug, Default, Serialize)]
pub struct Transaction {
    pub id: String,
    pub account_id: String,
    pub date: String,
    pub amount: i64,
    pub payee_id: Option<String>,
    pub category_id: Option<String>,
    pub memo: Option<String>,
    pub cleared: Cleared,
    pub approved: bool,
    pub flag_color: Option<FlagColor>,
    /// For a transfer, the other half's account and id (SPEC 9.1).
    pub transfer_account_id: Option<String>,
    pub transfer_transaction_id: Option<String>,
    /// In the order given; empty unless it's a split (SPEC 10).
    pub subtransactions: Vec<Subtransaction>,
    /// The bank transaction it was imported from or matched to (SPEC 17.9).
    pub import: Option<Import>,
}

/// A bank transaction's data, after its statement's options (SPEC 17.8).
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct BankData {
    /// Its import id.
    pub id: String,
    pub date: String,
    pub amount: i64,
    pub payee_name: Option<String>,
    pub memo: Option<String>,
}

/// A transaction's `import` (SPEC 17.9).
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Import {
    #[serde(flatten)]
    pub data: BankData,
    /// Whether it was matched to a transaction already in the account
    /// (SPEC 17.10), rather than created from the bank's (SPEC 17.11).
    pub matched: bool,
}

impl Transaction {
    /// Whether the transaction or one of its subtransactions has the
    /// category (SPEC 8.5, 13.2).
    pub fn has_category(&self, category_id: &str) -> bool {
        self.category_id.as_deref() == Some(category_id)
            || self
                .subtransactions
                .iter()
                .any(|s| s.category_id == category_id)
    }

    /// Moves the transaction and its subtransactions from category `from`
    /// to `to`, changing nothing else (SPEC 13.3).
    fn recategorize(&mut self, from: &str, to: &str) {
        if self.category_id.as_deref() == Some(from) {
            self.category_id = Some(to.to_owned());
        }
        for s in self
            .subtransactions
            .iter_mut()
            .filter(|s| s.category_id == from)
        {
            to.clone_into(&mut s.category_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Plans

/// What the API shows of a plan (SPEC 4.1).
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct PlanInfo {
    pub id: String,
    pub name: String,
    pub currency_code: String,
}

/// Each category's assigned amounts by month (SPEC 11.2); a month missing
/// from a category's map has 0 assigned.
pub type Assigned<'a> = HashMap<&'a str, BTreeMap<Month, i64>>;

#[derive(Clone, Debug)]
pub struct Plan {
    pub info: PlanInfo,
    /// When the plan was created, in microseconds since the Unix epoch.
    /// Plans are listed in this order (SPEC 4.2, 15.5).
    pub created_at: i64,
    pub accounts: Table<Account>,
    /// Transfer payees included.
    pub payees: Table<Payee>,
    /// The internal Credit Card Payments group first, then the plan's own
    /// groups.
    pub groups: Table<Group>,
    /// In the order they joined their group, so listing a group's
    /// categories in this order lists them in its order (SPEC 7.6).
    pub categories: Table<Category>,
    pub transactions: Table<Transaction>,
    /// Every category's assignments, in the order they were made (SPEC 18.2).
    pub assignments: Table<Assignment>,
    /// The import ids each account has had, by account id (SPEC 17.9).
    pub import_ids: HashMap<String, BTreeSet<String>>,
    /// What the last import of each statement used (SPEC 17.13).
    pub import_options: HashMap<StatementKey, ImportOptions>,
}

impl Plan {
    pub fn new(name: String, currency_code: String, created_at: i64) -> Self {
        let id = new_id();
        let internal = Group {
            id: new_id(),
            name: CREDIT_CARD_PAYMENTS.into(),
            hidden: false,
            internal: true,
        };
        Self {
            info: PlanInfo {
                id,
                name,
                currency_code,
            },
            created_at,
            accounts: Table::new(),
            payees: Table::new(),
            groups: Table::from([(internal.id.clone(), internal)]),
            categories: Table::new(),
            transactions: Table::new(),
            assignments: Table::new(),
            import_ids: HashMap::new(),
            import_options: HashMap::new(),
        }
    }

    // --- accounts ---------------------------------------------------------

    /// An account's balances (SPEC 5.4).
    pub fn balances(&self, account_id: &str) -> Balances {
        let mut b = Balances::default();
        for t in self
            .transactions
            .values()
            .filter(|t| t.account_id == account_id)
        {
            b.balance += t.amount;
            match t.cleared {
                Cleared::Cleared | Cleared::Reconciled => b.cleared_balance += t.amount,
                Cleared::Uncleared => b.uncleared_balance += t.amount,
            }
        }
        b
    }

    pub fn account_view(&self, account_id: &str) -> AccountView {
        let account = self
            .accounts
            .get(account_id)
            .expect("the caller found the account");
        AccountView {
            account: account.clone(),
            on_budget: account.kind.on_budget(),
            balances: self.balances(account_id),
        }
    }

    /// Creates an account with its transfer payee, its payment category if
    /// it's a credit account, and its starting balance transaction if
    /// `starting` is a non-zero amount and its date, in that order (SPEC 5.5).
    /// Returns the account's id.
    pub fn create_account(
        &mut self,
        name: String,
        kind: AccountType,
        note: Option<String>,
        starting: Option<(i64, String)>,
    ) -> String {
        let account_id = new_id();
        let transfer_payee_id = new_id();
        self.payees.insert(
            transfer_payee_id.clone(),
            Payee {
                id: transfer_payee_id.clone(),
                name: transfer_payee_name(&name),
                transfer_account_id: Some(account_id.clone()),
            },
        );
        let payment_category_id = kind.is_credit().then(|| {
            let (group_id, group) = self
                .groups
                .first()
                .expect("every plan has the internal group");
            debug_assert!(group.internal);
            let category = Category {
                id: new_id(),
                group_id: group_id.clone(),
                name: name.clone(),
                hidden: false,
                note: None,
                payment_account_id: Some(account_id.clone()),
                targets: Vec::new(),
            };
            let id = category.id.clone();
            self.categories.insert(id.clone(), category);
            id
        });
        self.accounts.insert(
            account_id.clone(),
            Account {
                id: account_id.clone(),
                name,
                kind,
                closed: false,
                note,
                transfer_payee_id,
                payment_category_id,
            },
        );
        if let Some((amount, date)) = starting.filter(|(amount, _)| *amount != 0) {
            self.add_balance_transaction(
                &account_id,
                kind,
                date,
                amount,
                STARTING_BALANCE_PAYEE,
                Cleared::Cleared,
            );
        }
        account_id
    }

    /// Adds a transaction the server makes to set an account's balance: a
    /// starting balance (SPEC 5.3) or a reconciliation adjustment
    /// (SPEC 14.2). Its payee is found or created by name (SPEC 6.5), its
    /// memo is `null`, it's approved with no flag, and its category is
    /// `ready_to_assign` in an on-budget account and none in a tracking one.
    /// Returns its id.
    fn add_balance_transaction(
        &mut self,
        account_id: &str,
        kind: AccountType,
        date: String,
        amount: i64,
        payee_name: &str,
        cleared: Cleared,
    ) -> String {
        let transaction = Transaction {
            id: new_id(),
            account_id: account_id.to_owned(),
            date,
            amount,
            payee_id: Some(self.payee_by_name(payee_name)),
            category_id: kind.on_budget().then(|| READY_TO_ASSIGN.to_owned()),
            cleared,
            approved: true,
            ..Default::default()
        };
        let id = transaction.id.clone();
        self.transactions.insert(id.clone(), transaction);
        id
    }

    /// Renames an account, its transfer payee and its payment category
    /// (SPEC 5.7).
    pub fn rename_account(&mut self, account_id: &str, name: String) {
        let account = self
            .accounts
            .get_mut(account_id)
            .expect("the caller found the account");
        let payee = self
            .payees
            .get_mut(&account.transfer_payee_id)
            .expect("an account's transfer payee exists");
        payee.name = transfer_payee_name(&name);
        if let Some(category_id) = &account.payment_category_id {
            let category = self
                .categories
                .get_mut(category_id)
                .expect("an account's payment category exists");
            category.name.clone_from(&name);
        }
        account.name = name;
    }

    /// Reconciles an account (SPEC 14.2, 14.3): adds an adjustment of
    /// `adjustment` milliunits dated `date` unless it's 0, then marks every
    /// cleared transaction in the account reconciled. The caller checks the
    /// adjustment is an amount (SPEC 3.7). Returns the adjustment's id.
    pub fn reconcile(&mut self, account_id: &str, adjustment: i64, date: String) -> Option<String> {
        let kind = self
            .accounts
            .get(account_id)
            .expect("the caller found the account")
            .kind;
        let adjustment_id = (adjustment != 0).then(|| {
            self.add_balance_transaction(
                account_id,
                kind,
                date,
                adjustment,
                RECONCILIATION_PAYEE,
                Cleared::Reconciled,
            )
        });
        for t in self
            .transactions
            .values_mut()
            .filter(|t| t.account_id == account_id && t.cleared == Cleared::Cleared)
        {
            t.cleared = Cleared::Reconciled;
        }
        adjustment_id
    }

    pub fn account_has_transactions(&self, account_id: &str) -> bool {
        self.transactions
            .values()
            .any(|t| t.account_id == account_id)
    }

    /// Deletes an account, its transfer payee and its payment category
    /// (SPEC 5.9), and what imports kept about it (SPEC 17.6, 17.9). The
    /// caller checks it has no transactions.
    pub fn delete_account(&mut self, account_id: &str) {
        let account = self
            .accounts
            .shift_remove(account_id)
            .expect("the caller found the account");
        self.payees.shift_remove(&account.transfer_payee_id);
        self.import_ids.remove(account_id);
        self.import_options
            .retain(|_, options| options.account_id != account_id);
        if let Some(category_id) = account.payment_category_id {
            self.assignments.retain(|_, a| a.category_id != category_id);
            self.categories.shift_remove(&category_id);
        }
    }

    // --- payees -----------------------------------------------------------

    /// The payee, not a transfer payee, named exactly `name`.
    pub fn find_payee_by_name(&self, name: &str) -> Option<&Payee> {
        self.payees
            .values()
            .find(|p| !p.is_transfer() && p.name == name)
    }

    /// Finds the payee, not a transfer payee, named exactly `name`, creating
    /// it if there is none (SPEC 6.5). Returns its id.
    pub fn payee_by_name(&mut self, name: &str) -> String {
        if let Some(p) = self.find_payee_by_name(name) {
            return p.id.clone();
        }
        let id = new_id();
        self.payees.insert(
            id.clone(),
            Payee {
                id: id.clone(),
                name: name.to_owned(),
                transfer_account_id: None,
            },
        );
        id
    }

    pub fn payee_in_use(&self, payee_id: &str) -> bool {
        self.transactions
            .values()
            .any(|t| t.payee_id.as_deref() == Some(payee_id))
    }

    // --- categories -------------------------------------------------------

    /// A group's categories, in the order they joined it.
    pub fn group_categories<'a>(&'a self, group_id: &'a str) -> impl Iterator<Item = &'a Category> {
        self.categories
            .values()
            .filter(move |c| c.group_id == group_id)
    }

    pub fn group_view(&self, group: &Group) -> GroupView {
        GroupView {
            group: group.clone(),
            categories: self
                .group_categories(&group.id)
                .map(Category::view)
                .collect(),
        }
    }

    // --- assignments --------------------------------------------------------

    /// A category's assignments for a month, in the order they were made
    /// (SPEC 18.2).
    pub fn assignments_in<'a>(
        &'a self,
        category_id: &'a str,
        month: Month,
    ) -> impl Iterator<Item = &'a Assignment> {
        self.assignments
            .values()
            .filter(move |a| a.category_id == category_id && a.month == month)
    }

    /// Every category's `assigned` by month: the sum of its assignments
    /// (SPEC 18.2).
    pub fn assigned(&self) -> Assigned<'_> {
        let mut assigned = Assigned::new();
        for a in self.assignments.values() {
            *assigned
                .entry(a.category_id.as_str())
                .or_default()
                .entry(a.month)
                .or_insert(0) += a.amount;
        }
        assigned
    }

    /// Sets a category's `assigned` for a month (SPEC 11.2) by making an
    /// assignment dated `today` of the difference, unless there is none
    /// (SPEC 18.2).
    pub fn assign(&mut self, category_id: &str, month: Month, assigned: i64, today: &str) {
        let before: i64 = self
            .assignments_in(category_id, month)
            .map(|a| a.amount)
            .sum();
        if assigned == before {
            return;
        }
        let assignment = Assignment {
            id: new_id(),
            category_id: category_id.to_owned(),
            month,
            date: today.to_owned(),
            amount: assigned - before,
        };
        self.assignments.insert(assignment.id.clone(), assignment);
    }

    /// Whether a transaction is uncategorized (SPEC 16.1): in an on-budget
    /// account, without a category, and neither a split nor a transfer
    /// between two on-budget accounts.
    pub fn is_uncategorized(&self, t: &Transaction) -> bool {
        let on_budget = |account_id: &str| {
            self.accounts
                .get(account_id)
                .expect("a transaction's accounts exist")
                .kind
                .on_budget()
        };
        on_budget(&t.account_id)
            && t.category_id.is_none()
            && t.subtransactions.is_empty()
            && !t.transfer_account_id.as_deref().is_some_and(on_budget)
    }

    pub fn category_in_use(&self, category_id: &str) -> bool {
        self.transactions
            .values()
            .any(|t| t.has_category(category_id))
    }

    /// Deletes a user category with its targets (SPEC 13.3, 18.7). With
    /// `reassign_to`, its transactions, subtransactions and assignments move
    /// to that category, which adds its `assigned` to that category's month
    /// by month; assignments keep their place in the order they were made.
    /// Without it, its assignments are dropped; the caller checks it isn't
    /// in use.
    pub fn delete_category(&mut self, category_id: &str, reassign_to: Option<&str>) {
        self.categories
            .shift_remove(category_id)
            .expect("the caller found the category");
        let Some(to) = reassign_to else {
            debug_assert!(!self.category_in_use(category_id));
            self.assignments.retain(|_, a| a.category_id != category_id);
            return;
        };
        for t in self.transactions.values_mut() {
            t.recategorize(category_id, to);
        }
        for a in self
            .assignments
            .values_mut()
            .filter(|a| a.category_id == category_id)
        {
            to.clone_into(&mut a.category_id);
        }
    }
}
