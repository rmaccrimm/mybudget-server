//! The data the server keeps, and its JSON views.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::error::ApiError;

/// The category id of Inflow: Ready to Assign (SPEC 7.7). Generated ids
/// always contain a `-`, so none equals it.
pub const READY_TO_ASSIGN: &str = "ready_to_assign";
/// The payee of starting balance transactions (SPEC 5.3).
pub const STARTING_BALANCE_PAYEE: &str = "Starting Balance";
/// The internal group's name (SPEC 7.1).
pub const CREDIT_CARD_PAYMENTS: &str = "Credit Card Payments";

pub fn transfer_payee_name(account_name: &str) -> String {
    format!("Transfer : {account_name}")
}

/// Generates ids unique across the server, never reused (SPEC 3.5).
pub struct Ids {
    prefix: String,
    next: u64,
}

impl Ids {
    fn new() -> Self {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        Self { prefix: format!("{:x}", nanos as u64), next: 1 }
    }

    pub fn next(&mut self) -> String {
        let id = format!("{}-{}", self.prefix, self.next);
        self.next += 1;
        id
    }
}

pub struct Store {
    pub ids: Ids,
    /// Oldest first.
    pub plans: Vec<Plan>,
}

impl Store {
    pub fn new() -> Self {
        Self { ids: Ids::new(), plans: Vec::new() }
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

pub fn plan_index(plans: &[Plan], id: &str) -> Result<usize, ApiError> {
    plans.iter().position(|p| p.id == id).ok_or_else(ApiError::not_found)
}

pub fn plan<'a>(plans: &'a [Plan], id: &str) -> Result<&'a Plan, ApiError> {
    Ok(&plans[plan_index(plans, id)?])
}

pub fn plan_mut<'a>(plans: &'a mut [Plan], id: &str) -> Result<&'a mut Plan, ApiError> {
    let i = plan_index(plans, id)?;
    Ok(&mut plans[i])
}

// ---------------------------------------------------------------------------
// Entities

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    const ALL: [AccountType; 7] = [
        Self::Checking,
        Self::Savings,
        Self::Cash,
        Self::CreditCard,
        Self::LineOfCredit,
        Self::OtherAsset,
        Self::OtherLiability,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Checking => "checking",
            Self::Savings => "savings",
            Self::Cash => "cash",
            Self::CreditCard => "creditCard",
            Self::LineOfCredit => "lineOfCredit",
            Self::OtherAsset => "otherAsset",
            Self::OtherLiability => "otherLiability",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    pub fn is_credit(self) -> bool {
        matches!(self, Self::CreditCard | Self::LineOfCredit)
    }

    pub fn on_budget(self) -> bool {
        !matches!(self, Self::OtherAsset | Self::OtherLiability)
    }
}

pub struct Account {
    pub id: String,
    pub name: String,
    pub kind: AccountType,
    pub closed: bool,
    pub note: Option<String>,
    pub transfer_payee_id: String,
    pub payment_category_id: Option<String>,
}

pub struct Payee {
    pub id: String,
    pub name: String,
    pub transfer_account_id: Option<String>,
}

impl Payee {
    pub fn is_transfer(&self) -> bool {
        self.transfer_account_id.is_some()
    }

    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "transfer_account_id": self.transfer_account_id })
    }
}

pub struct Group {
    pub id: String,
    pub name: String,
    pub hidden: bool,
    pub internal: bool,
    /// In the order they joined the group.
    pub category_ids: Vec<String>,
}

pub struct Category {
    pub id: String,
    pub group_id: String,
    pub name: String,
    pub hidden: bool,
    pub note: Option<String>,
    pub payment_account_id: Option<String>,
}

impl Category {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "group_id": self.group_id,
            "name": self.name,
            "hidden": self.hidden,
            "note": self.note,
            "payment_account_id": self.payment_account_id,
        })
    }
}

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cleared {
    Uncleared,
    Cleared,
    Reconciled,
}

impl Cleared {
    const ALL: [Cleared; 3] = [Self::Uncleared, Self::Cleared, Self::Reconciled];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uncleared => "uncleared",
            Self::Cleared => "cleared",
            Self::Reconciled => "reconciled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// A new transaction's default: money in a wallet clears at once
    /// (SPEC 8.1).
    pub fn default_for(kind: AccountType) -> Self {
        if kind == AccountType::Cash { Self::Cleared } else { Self::Uncleared }
    }
}

/// Flag colours (SPEC 8.1).
pub const FLAG_COLORS: [&str; 6] = ["red", "orange", "yellow", "green", "blue", "purple"];

#[derive(Clone, Debug)]
pub struct Subtransaction {
    pub amount: i64,
    pub category_id: String,
    pub memo: Option<String>,
}

impl Subtransaction {
    pub fn to_json(&self) -> Value {
        json!({ "amount": self.amount, "category_id": self.category_id, "memo": self.memo })
    }
}

#[derive(Clone, Debug)]
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
    pub flag_color: Option<String>,
    /// For a transfer, the other half's account and id (SPEC 9.1).
    pub transfer_account_id: Option<String>,
    pub transfer_transaction_id: Option<String>,
    /// In the order given; empty unless it's a split (SPEC 10).
    pub subtransactions: Vec<Subtransaction>,
}

impl Transaction {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "account_id": self.account_id,
            "date": self.date,
            "amount": self.amount,
            "payee_id": self.payee_id,
            "category_id": self.category_id,
            "memo": self.memo,
            "cleared": self.cleared.as_str(),
            "approved": self.approved,
            "flag_color": self.flag_color,
            "transfer_account_id": self.transfer_account_id,
            "transfer_transaction_id": self.transfer_transaction_id,
            "subtransactions": self.subtransactions.iter().map(Subtransaction::to_json).collect::<Vec<_>>(),
        })
    }
}

// ---------------------------------------------------------------------------
// Plans

pub struct Plan {
    pub id: String,
    pub name: String,
    pub currency_code: String,
    /// Oldest first.
    pub accounts: Vec<Account>,
    /// Oldest first, transfer payees included.
    pub payees: Vec<Payee>,
    /// The internal Credit Card Payments group first, then the plan's own
    /// groups, oldest first.
    pub groups: Vec<Group>,
    pub categories: Vec<Category>,
    /// Oldest first.
    pub transactions: Vec<Transaction>,
}

#[derive(Default, Clone, Copy)]
pub struct Balances {
    pub balance: i64,
    pub cleared: i64,
    pub uncleared: i64,
}

impl Plan {
    pub fn new(ids: &mut Ids, name: String, currency_code: String) -> Self {
        let id = ids.next();
        let internal = Group {
            id: ids.next(),
            name: CREDIT_CARD_PAYMENTS.into(),
            hidden: false,
            internal: true,
            category_ids: Vec::new(),
        };
        Self {
            id,
            name,
            currency_code,
            accounts: Vec::new(),
            payees: Vec::new(),
            groups: vec![internal],
            categories: Vec::new(),
            transactions: Vec::new(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "currency_code": self.currency_code })
    }

    // --- accounts ---------------------------------------------------------

    pub fn account_index(&self, id: &str) -> Result<usize, ApiError> {
        self.accounts.iter().position(|a| a.id == id).ok_or_else(ApiError::not_found)
    }

    /// An account's balances (SPEC 5.4).
    pub fn balances(&self, account_id: &str) -> Balances {
        let mut b = Balances::default();
        for t in self.transactions.iter().filter(|t| t.account_id == account_id) {
            b.balance += t.amount;
            match t.cleared {
                Cleared::Cleared | Cleared::Reconciled => b.cleared += t.amount,
                Cleared::Uncleared => b.uncleared += t.amount,
            }
        }
        b
    }

    pub fn account_json(&self, a: &Account) -> Value {
        let b = self.balances(&a.id);
        json!({
            "id": a.id,
            "name": a.name,
            "type": a.kind.as_str(),
            "on_budget": a.kind.on_budget(),
            "closed": a.closed,
            "note": a.note,
            "balance": b.balance,
            "cleared_balance": b.cleared,
            "uncleared_balance": b.uncleared,
            "transfer_payee_id": a.transfer_payee_id,
            "payment_category_id": a.payment_category_id,
        })
    }

    /// Creates an account with its transfer payee, its payment category if
    /// it's a credit account, and its starting balance transaction if
    /// `starting` is a non-zero amount and its date, in that order (SPEC 5.5).
    pub fn create_account(
        &mut self,
        ids: &mut Ids,
        name: String,
        kind: AccountType,
        note: Option<String>,
        starting: Option<(i64, String)>,
    ) -> usize {
        let account_id = ids.next();
        let transfer_payee_id = ids.next();
        self.payees.push(Payee {
            id: transfer_payee_id.clone(),
            name: transfer_payee_name(&name),
            transfer_account_id: Some(account_id.clone()),
        });
        let payment_category_id = if kind.is_credit() {
            let id = ids.next();
            let group = &mut self.groups[0];
            debug_assert!(group.internal);
            group.category_ids.push(id.clone());
            self.categories.push(Category {
                id: id.clone(),
                group_id: group.id.clone(),
                name: name.clone(),
                hidden: false,
                note: None,
                payment_account_id: Some(account_id.clone()),
            });
            Some(id)
        } else {
            None
        };
        self.accounts.push(Account {
            id: account_id.clone(),
            name,
            kind,
            closed: false,
            note,
            transfer_payee_id,
            payment_category_id,
        });
        if let Some((amount, date)) = starting.filter(|(amount, _)| *amount != 0) {
            let payee_id = self.payee_by_name(ids, STARTING_BALANCE_PAYEE);
            let category_id = kind.on_budget().then(|| READY_TO_ASSIGN.to_owned());
            self.transactions.push(Transaction {
                id: ids.next(),
                account_id,
                date,
                amount,
                payee_id: Some(payee_id),
                category_id,
                memo: None,
                cleared: Cleared::Cleared,
                approved: true,
                flag_color: None,
                transfer_account_id: None,
                transfer_transaction_id: None,
                subtransactions: Vec::new(),
            });
        }
        self.accounts.len() - 1
    }

    /// Renames an account, its transfer payee and its payment category
    /// (SPEC 5.7).
    pub fn rename_account(&mut self, index: usize, name: String) {
        let account = &mut self.accounts[index];
        if let Some(p) = self.payees.iter_mut().find(|p| p.id == account.transfer_payee_id) {
            p.name = transfer_payee_name(&name);
        }
        if let Some(cid) = &account.payment_category_id
            && let Some(c) = self.categories.iter_mut().find(|c| &c.id == cid)
        {
            c.name = name.clone();
        }
        account.name = name;
    }

    pub fn account_has_transactions(&self, account_id: &str) -> bool {
        self.transactions.iter().any(|t| t.account_id == account_id)
    }

    /// Deletes an account, its transfer payee and its payment category
    /// (SPEC 5.9). The caller checks it has no transactions.
    pub fn delete_account(&mut self, index: usize) {
        let account = self.accounts.remove(index);
        self.payees.retain(|p| p.id != account.transfer_payee_id);
        if let Some(cid) = account.payment_category_id {
            self.remove_category(&cid);
        }
    }

    // --- payees -----------------------------------------------------------

    pub fn payee_index(&self, id: &str) -> Result<usize, ApiError> {
        self.payees.iter().position(|p| p.id == id).ok_or_else(ApiError::not_found)
    }

    /// The payee, not a transfer payee, named exactly `name`.
    pub fn find_payee_by_name(&self, name: &str) -> Option<&Payee> {
        self.payees.iter().find(|p| !p.is_transfer() && p.name == name)
    }

    /// Finds the payee, not a transfer payee, named exactly `name`, creating
    /// it if there is none (SPEC 6.5). Returns its id.
    pub fn payee_by_name(&mut self, ids: &mut Ids, name: &str) -> String {
        if let Some(p) = self.find_payee_by_name(name) {
            return p.id.clone();
        }
        let id = ids.next();
        self.payees.push(Payee { id: id.clone(), name: name.to_owned(), transfer_account_id: None });
        id
    }

    pub fn payee_in_use(&self, payee_id: &str) -> bool {
        self.transactions.iter().any(|t| t.payee_id.as_deref() == Some(payee_id))
    }

    // --- transactions -----------------------------------------------------

    pub fn transaction_index(&self, id: &str) -> Result<usize, ApiError> {
        self.transactions.iter().position(|t| t.id == id).ok_or_else(ApiError::not_found)
    }

    // --- categories -------------------------------------------------------

    pub fn group_index(&self, id: &str) -> Result<usize, ApiError> {
        self.groups.iter().position(|g| g.id == id).ok_or_else(ApiError::not_found)
    }

    /// One of the plan's own groups, which excludes the internal one.
    pub fn own_group_index(&self, id: &str) -> Option<usize> {
        self.groups.iter().position(|g| g.id == id && !g.internal)
    }

    pub fn category_index(&self, id: &str) -> Result<usize, ApiError> {
        self.categories.iter().position(|c| c.id == id).ok_or_else(ApiError::not_found)
    }

    pub fn group_json(&self, g: &Group) -> Value {
        let categories: Vec<Value> = g
            .category_ids
            .iter()
            .filter_map(|id| self.categories.iter().find(|c| &c.id == id))
            .map(Category::to_json)
            .collect();
        json!({
            "id": g.id,
            "name": g.name,
            "hidden": g.hidden,
            "internal": g.internal,
            "categories": categories,
        })
    }

    /// Moves a category to the end of another group.
    pub fn move_category(&mut self, index: usize, to_group: usize) {
        let category = &mut self.categories[index];
        if let Some(from) = self.groups.iter_mut().find(|g| g.id == category.group_id) {
            from.category_ids.retain(|id| id != &category.id);
        }
        let to = &mut self.groups[to_group];
        to.category_ids.push(category.id.clone());
        category.group_id = to.id.clone();
    }

    pub fn remove_category(&mut self, id: &str) {
        self.categories.retain(|c| c.id != id);
        for g in &mut self.groups {
            g.category_ids.retain(|c| c != id);
        }
    }
}
