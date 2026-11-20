//! Importing OFX files (SPEC 17.1, 17.6, 17.7).

use std::collections::HashSet;

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;

use crate::AppState;
use crate::error::ApiError;
use crate::fields::{self, Body};
use crate::import::{Duplicate, ImportOptions, StatementRequest, import_ids};
use crate::json::Json as JsonValue;
use crate::model::{ById, Plan, Transaction};
use crate::ofx::{self, BankTransaction, Statement};

/// `file`: an OFX file in base64, with RFC 4648's standard alphabet, `=`
/// padding and no white space (SPEC 17.1), read into its statements.
fn file(field: &str, v: &JsonValue) -> Result<Vec<Statement>, ApiError> {
    let bytes = STANDARD
        .decode(fields::string(field, v)?)
        .map_err(|e| ApiError::bad_request(format!("{field} isn't base64: {e}")))?;
    ofx::read(&bytes).map_err(|e| ApiError::bad_request(format!("{field} isn't OFX: {e}")))
}

// ---------------------------------------------------------------------------
// Preview

#[derive(Serialize)]
struct Preview<'a> {
    statements: Vec<StatementPreview<'a>>,
}

/// A statement as SPEC 17.6 shows it.
#[derive(Serialize)]
struct StatementPreview<'a> {
    #[serde(flatten)]
    statement: &'a Statement,
    account_id: Option<&'a str>,
    swap_payee_memo: bool,
    invert_amounts: bool,
    transactions: Vec<TransactionPreview<'a>>,
}

#[derive(Serialize)]
struct TransactionPreview<'a> {
    import_id: String,
    #[serde(flatten)]
    transaction: &'a BankTransaction,
}

impl<'a> StatementPreview<'a> {
    fn new(plan: &'a Plan, statement: &'a Statement) -> Self {
        let remembered = plan.remembered(statement);
        Self {
            statement,
            account_id: remembered.map(|o| o.account_id.as_str()),
            swap_payee_memo: remembered.is_some_and(|o| o.swap_payee_memo),
            invert_amounts: remembered.is_some_and(|o| o.invert_amounts),
            transactions: import_ids(statement)
                .into_iter()
                .zip(&statement.transactions)
                .map(|(import_id, transaction)| TransactionPreview {
                    import_id,
                    transaction,
                })
                .collect(),
        }
    }
}

pub async fn preview(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let store = state.store();
    let plan = store.plan(&plan_id)?;
    let statements = Body::parse(&body)?.required("file", file)?;
    let preview = Preview {
        statements: statements
            .iter()
            .map(|s| StatementPreview::new(plan, s))
            .collect(),
    };
    // Serialized here, while the plan it borrows from is locked.
    Ok(Json(preview).into_response())
}

// ---------------------------------------------------------------------------
// Import

/// An entry of `statements`: `null` to skip a statement, or how to import it
/// (SPEC 17.7).
fn statement_request(
    plan: &Plan,
    field: &str,
    v: &JsonValue,
) -> Result<Option<StatementRequest>, ApiError> {
    if *v == JsonValue::Null {
        return Ok(None);
    }
    let entry = Body::object(field, v)?;
    let account_id = entry.required("account_id", fields::string)?;
    plan.accounts.referenced("account_id", &account_id)?;
    Ok(Some(StatementRequest {
        options: ImportOptions {
            account_id,
            swap_payee_memo: entry
                .optional("swap_payee_memo", fields::boolean)?
                .unwrap_or(false),
            invert_amounts: entry
                .optional("invert_amounts", fields::boolean)?
                .unwrap_or(false),
        },
        not_duplicates: entry
            .optional("not_duplicates", |f, v| fields::array(f, v, fields::string))?
            .map(HashSet::from_iter)
            .unwrap_or_default(),
    }))
}

#[derive(Serialize)]
struct Imported {
    statements: Vec<Option<StatementImported>>,
}

/// What importing a statement did (SPEC 17.7).
#[derive(Serialize)]
struct StatementImported {
    account_id: String,
    imported: Vec<Transaction>,
    matched: Vec<Transaction>,
    duplicates: Vec<Duplicate>,
}

pub async fn import(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    let body = Body::parse(&body)?;
    let statements = body.required("file", file)?;
    let requests = body.required("statements", |f, v| {
        fields::array(f, v, |f, v| statement_request(plan, f, v))
    })?;
    if requests.len() != statements.len() {
        return Err(ApiError::bad_request(format!(
            "statements must have one entry for each of the file's {} statements",
            statements.len()
        )));
    }

    // Everything is checked: from here on only the save can fail
    // (SPEC 3.11, 15.10).
    let imported = store.change_plan(&plan_id, |plan| {
        let results = plan.import(&statements, requests);
        let transactions =
            |ids: Vec<String>| ids.iter().map(|id| plan.transactions[id].clone()).collect();
        Imported {
            statements: results
                .into_iter()
                .map(|result| {
                    result.map(|r| StatementImported {
                        account_id: r.account_id,
                        imported: transactions(r.imported),
                        matched: transactions(r.matched),
                        duplicates: r.duplicates,
                    })
                })
                .collect(),
        }
    })?;
    Ok(Json(imported))
}
