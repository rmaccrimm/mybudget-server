//! Importing bank transactions into a plan (SPEC 17.6 to 17.14): which are
//! duplicates, which match a transaction already in the account, and the
//! transactions the rest create.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::calendar::days_apart;
use crate::fields::{MAX_NOTE, MAX_PAYEE_NAME};
use crate::model::{
    BankData, Cleared, Import, Plan, READY_TO_ASSIGN, RECONCILIATION_PAYEE, STARTING_BALANCE_PAYEE,
    Transaction, new_id,
};
use crate::ofx::{BankTransaction, Statement, StatementType};

/// What a statement is remembered under (SPEC 17.13).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct StatementKey {
    pub kind: StatementType,
    pub bank_id: Option<String>,
    pub account_number: String,
}

impl StatementKey {
    pub fn of(statement: &Statement) -> Self {
        Self {
            kind: statement.kind,
            bank_id: statement.bank_id.clone(),
            account_number: statement.account_number.clone(),
        }
    }
}

/// How a statement is imported (SPEC 17.7), and what an import remembers of
/// it (SPEC 17.13).
#[derive(Clone, PartialEq, Debug)]
pub struct ImportOptions {
    pub account_id: String,
    pub swap_payee_memo: bool,
    pub invert_amounts: bool,
}

/// What an import is asked to do with one statement (SPEC 17.7).
pub struct StatementRequest {
    pub options: ImportOptions,
    /// Import ids that skip the second duplicate check (SPEC 17.9).
    pub not_duplicates: HashSet<String>,
}

/// What importing a statement did (SPEC 17.7), by transaction id.
pub struct StatementResult {
    pub account_id: String,
    pub imported: Vec<String>,
    pub matched: Vec<String>,
    pub duplicates: Vec<Duplicate>,
}

/// A bank transaction skipped as a duplicate, and the transaction it
/// duplicates (SPEC 17.9).
#[derive(Serialize)]
pub struct Duplicate {
    #[serde(flatten)]
    pub data: BankData,
    pub transaction_id: Option<String>,
}

/// The import id of each of a statement's transactions (SPEC 17.8): its
/// `FITID`, or else its amount as the file has it, its date and a count.
pub fn import_ids(statement: &Statement) -> Vec<String> {
    let mut counts: HashMap<(i64, &str), usize> = HashMap::new();
    statement
        .transactions
        .iter()
        .map(|t| {
            if let Some(fitid) = &t.fitid {
                return fitid.clone();
            }
            let count = counts.entry((t.amount, &t.date)).or_default();
            *count += 1;
            format!("{}:{}:{count}", t.amount, t.date)
        })
        .collect()
}

/// The first `max` characters of `text`.
fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// A bank transaction's data after its statement's options (SPEC 17.8).
fn bank_data(t: &BankTransaction, import_id: String, options: &ImportOptions) -> BankData {
    let (payee, memo) = if options.swap_payee_memo {
        (&t.memo, &t.name)
    } else {
        (&t.name, &t.memo)
    };
    BankData {
        id: import_id,
        date: t.date.clone(),
        amount: if options.invert_amounts {
            -t.amount
        } else {
            t.amount
        },
        payee_name: payee
            .as_deref()
            .map(|p| cut(p, MAX_PAYEE_NAME).trim().to_owned())
            .filter(|p| !p.is_empty()),
        memo: memo.as_deref().map(|m| cut(m, MAX_NOTE)),
    }
}

/// What an import keeps track of across its statements (SPEC 17.9).
struct Progress {
    /// Transactions imported or matched before the import began that no bank
    /// transaction in it duplicates yet: the second check's candidates.
    unpaired: HashSet<String>,
    /// The import ids of the import's bank transactions so far, by account.
    seen: HashMap<String, HashSet<String>>,
}

/// What happens to a bank transaction (SPEC 17.7).
enum Outcome {
    /// Its import id is one the account has had (SPEC 17.9's first check).
    HadId,
    /// It's paired with a transaction imported before (the second check).
    Paired(String),
    /// It's matched to this transaction (SPEC 17.10).
    Matched(String),
    /// It creates this transaction (SPEC 17.11).
    Imported(String),
}

/// Of `candidates`, the one dated closest to `date`, and then the one
/// created first (SPEC 17.9, 17.10).
fn closest<'a>(
    candidates: impl Iterator<Item = &'a Transaction>,
    date: &str,
) -> Option<&'a Transaction> {
    // `min_by_key` keeps the first of equal keys, and transactions are in
    // the order they were created.
    candidates.min_by_key(|t| days_apart(&t.date, date))
}

impl Plan {
    /// What the last import of a statement like this used, unless its
    /// account has been deleted (SPEC 17.6).
    pub fn remembered(&self, statement: &Statement) -> Option<&ImportOptions> {
        self.import_options.get(&StatementKey::of(statement))
    }

    /// Imports `statements`, each as `requests` says, or not at all for a
    /// `None` (SPEC 17.7). The caller checks each request's account exists.
    pub fn import(
        &mut self,
        statements: &[Statement],
        requests: Vec<Option<StatementRequest>>,
    ) -> Vec<Option<StatementResult>> {
        let mut progress = Progress {
            unpaired: self
                .transactions
                .values()
                .filter(|t| t.import.is_some())
                .map(|t| t.id.clone())
                .collect(),
            seen: HashMap::new(),
        };
        let outcomes: Vec<_> = statements
            .iter()
            .zip(requests)
            .map(|(statement, request)| {
                request.map(|request| self.import_statement(statement, request, &mut progress))
            })
            .collect();
        // Every transaction in the response is as it is after the whole
        // import, and so is a duplicate's transaction_id.
        outcomes
            .into_iter()
            .map(|outcome| outcome.map(|(account_id, outcome)| self.result(account_id, outcome)))
            .collect()
    }

    /// Imports one statement (SPEC 17.7 to 17.13), returning its account and
    /// each bank transaction with what happened to it, in file order.
    fn import_statement(
        &mut self,
        statement: &Statement,
        request: StatementRequest,
        progress: &mut Progress,
    ) -> (String, Vec<(BankData, Outcome)>) {
        let StatementRequest {
            options,
            not_duplicates,
        } = request;
        let account_id = options.account_id.clone();
        let bank: Vec<BankData> = statement
            .transactions
            .iter()
            .zip(import_ids(statement))
            .map(|(t, id)| bank_data(t, id, &options))
            .collect();

        // The first check runs over every transaction before the second.
        // An id counts as had from an earlier bank transaction in this
        // import into the account, too, whatever became of that one.
        let had = self.import_ids.get(&account_id);
        let seen = progress.seen.entry(account_id.clone()).or_default();
        let had_id: Vec<bool> = bank
            .iter()
            .map(|b| {
                let earlier_in_import = !seen.insert(b.id.clone());
                earlier_in_import || had.is_some_and(|had| had.contains(&b.id))
            })
            .collect();
        // A transaction that a first-check duplicate duplicates can't also be
        // paired by the second check.
        for (data, _) in bank.iter().zip(&had_id).filter(|(_, had_id)| **had_id) {
            if let Some(t) = self.imported_as(&account_id, &data.id) {
                progress.unpaired.remove(&t.id);
            }
        }

        let mut outcomes = Vec::with_capacity(bank.len());
        for (data, had_id) in bank.into_iter().zip(had_id) {
            let outcome = if had_id {
                Outcome::HadId
            } else if let Some(paired) = (!not_duplicates.contains(&data.id))
                .then(|| self.earlier_duplicate(&account_id, &data, &progress.unpaired))
                .flatten()
            {
                progress.unpaired.remove(&paired);
                Outcome::Paired(paired)
            } else {
                self.import_ids
                    .entry(account_id.clone())
                    .or_default()
                    .insert(data.id.clone());
                match self.match_for(&account_id, &data) {
                    Some(id) => {
                        self.mark_matched(&id, data.clone());
                        Outcome::Matched(id)
                    }
                    None => Outcome::Imported(self.add_imported(&account_id, data.clone())),
                }
            };
            outcomes.push((data, outcome));
        }
        self.import_options
            .insert(StatementKey::of(statement), options);
        (account_id, outcomes)
    }

    /// The account's transaction whose `import` has this import id, if any
    /// (SPEC 17.9's first check).
    fn imported_as(&self, account_id: &str, import_id: &str) -> Option<&Transaction> {
        self.transactions.values().find(|t| {
            t.account_id == account_id && t.import.as_ref().is_some_and(|i| i.data.id == import_id)
        })
    }

    /// The transaction imported or matched before this import that a bank
    /// transaction duplicates, if any (SPEC 17.9's second check).
    fn earlier_duplicate(
        &self,
        account_id: &str,
        data: &BankData,
        unpaired: &HashSet<String>,
    ) -> Option<String> {
        let candidates = self.transactions.values().filter(|t| {
            unpaired.contains(&t.id)
                && t.account_id == account_id
                && t.amount == data.amount
                && days_apart(&t.date, &data.date) <= 2
        });
        Some(closest(candidates, &data.date)?.id.clone())
    }

    /// The transaction entered by hand that a bank transaction matches, if
    /// any (SPEC 17.10).
    fn match_for(&self, account_id: &str, data: &BankData) -> Option<String> {
        let candidates = self.transactions.values().filter(|t| {
            t.account_id == account_id
                && t.import.is_none()
                && !t.payee_id.as_ref().is_some_and(|id| {
                    let name = &self.payees[id].name;
                    name == STARTING_BALANCE_PAYEE || name == RECONCILIATION_PAYEE
                })
                && t.amount == data.amount
                && days_apart(&t.date, &data.date) <= 10
        });
        Some(closest(candidates, &data.date)?.id.clone())
    }

    /// Matches a transaction to a bank transaction: only its `import`,
    /// `cleared` and `approved` change (SPEC 17.10).
    fn mark_matched(&mut self, transaction_id: &str, data: BankData) {
        let t = self
            .transactions
            .get_mut(transaction_id)
            .expect("the match is in the plan");
        t.import = Some(Import {
            data,
            matched: true,
        });
        if t.cleared == Cleared::Uncleared {
            t.cleared = Cleared::Cleared;
        }
        t.approved = true;
    }

    /// Creates a transaction from a bank transaction (SPEC 17.11), returning
    /// its id.
    fn add_imported(&mut self, account_id: &str, data: BankData) -> String {
        let payee_id = data.payee_name.as_deref().map(|n| self.payee_by_name(n));
        let category_id = self.imported_category(account_id, payee_id.as_deref(), data.amount);
        let t = Transaction {
            id: new_id(),
            account_id: account_id.to_owned(),
            date: data.date.clone(),
            amount: data.amount,
            payee_id,
            category_id,
            memo: data.memo.clone(),
            cleared: Cleared::Cleared,
            approved: false,
            import: Some(Import {
                data,
                matched: false,
            }),
            ..Default::default()
        };
        let id = t.id.clone();
        self.transactions.insert(id.clone(), t);
        id
    }

    /// An imported transaction's category (SPEC 17.12).
    fn imported_category(
        &self,
        account_id: &str,
        payee_id: Option<&str>,
        amount: i64,
    ) -> Option<String> {
        let kind = self.accounts[account_id].kind;
        if !kind.on_budget() {
            return None;
        }
        let payees_last = payee_id.and_then(|payee| {
            self.transactions
                .values()
                .rev()
                .filter(|t| t.payee_id.as_deref() == Some(payee))
                .find_map(|t| t.category_id.clone())
        });
        payees_last.or_else(|| (amount > 0 && kind.is_cash()).then(|| READY_TO_ASSIGN.to_owned()))
    }

    /// What importing a statement did, once the whole import is done.
    fn result(&self, account_id: String, outcomes: Vec<(BankData, Outcome)>) -> StatementResult {
        let mut result = StatementResult {
            account_id,
            imported: Vec::new(),
            matched: Vec::new(),
            duplicates: Vec::new(),
        };
        for (data, outcome) in outcomes {
            match outcome {
                Outcome::Imported(id) => result.imported.push(id),
                Outcome::Matched(id) => result.matched.push(id),
                Outcome::Paired(id) => result.duplicates.push(Duplicate {
                    data,
                    transaction_id: Some(id),
                }),
                Outcome::HadId => {
                    let transaction_id = self
                        .imported_as(&result.account_id, &data.id)
                        .map(|t| t.id.clone());
                    result.duplicates.push(Duplicate {
                        data,
                        transaction_id,
                    });
                }
            }
        }
        result
    }

    /// Undoes a match (SPEC 17.14): the transaction loses its `import`, and
    /// the bank transaction creates a new one. The caller checks the
    /// transaction was matched. Returns the new transaction's id.
    pub fn unmatch(&mut self, transaction_id: &str) -> String {
        let t = self
            .transactions
            .get_mut(transaction_id)
            .expect("the caller found the transaction");
        let import = t.import.take().expect("the caller checked it was matched");
        let account_id = t.account_id.clone();
        self.add_imported(&account_id, import.data)
    }
}
