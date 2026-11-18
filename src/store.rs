//! The plans the server holds, and the files they're saved in.
//!
//! Every change to a plan goes through [`Store::change_plan`], which saves it
//! before the plan in memory changes, so a change that can't be saved changes
//! nothing (SPEC 3.11, 15.10).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;

use crate::error::ApiError;
use crate::model::{ById, Plan, Table};
use crate::persistence::PlanFiles;

#[derive(Default)]
pub struct Store {
    /// Oldest first (SPEC 4.2).
    plans: Table<Plan>,
    /// Where each plan is saved. Without a data directory there's nowhere,
    /// and plans live only in memory (SPEC 15).
    files: Option<PlanFiles>,
}

/// The 500 for a change the server couldn't save (SPEC 15.10). The cause
/// goes to stderr, for whoever runs the server.
fn unsaved(plan_id: &str, cause: impl std::fmt::Display) -> ApiError {
    eprintln!("couldn't save plan {plan_id}: {cause}");
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "the server couldn't save the change",
    )
}

impl Store {
    /// The store for a data directory, holding the plans saved there
    /// (SPEC 15.1, 15.5).
    pub fn open(dir: &Path) -> Result<Self, String> {
        let (files, plans) = PlanFiles::open(dir)?;
        Ok(Self {
            plans: plans.into_iter().map(|p| (p.info.id.clone(), p)).collect(),
            files: Some(files),
        })
    }

    pub fn plans(&self) -> impl Iterator<Item = &Plan> {
        self.plans.values()
    }

    pub fn plan(&self, id: &str) -> Result<&Plan, ApiError> {
        self.plans.by_id(id)
    }

    /// A creation time later than every plan's, so the order plans are
    /// listed in follows from their creation times after a restart, even if
    /// a plan file came from a computer whose clock was ahead (SPEC 15.5).
    fn next_created_at(&self) -> i64 {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let now =
            i64::try_from(since_epoch.as_micros()).expect("the clock is before the year 294000");
        self.plans
            .values()
            .map(|p| p.created_at + 1)
            .fold(now, i64::max)
    }

    pub fn create_plan(&mut self, name: String, currency_code: String) -> Result<&Plan, ApiError> {
        let plan = Plan::new(name, currency_code, self.next_created_at());
        if let Some(files) = &mut self.files {
            files.create(&plan).map_err(|e| unsaved(&plan.info.id, e))?;
        }
        Ok(self.plans.entry(plan.info.id.clone()).or_insert(plan))
    }

    /// Makes `change` to the plan, saving it first. `change` can't fail: the
    /// caller has checked the request (SPEC 3.11), and only the save can
    /// still fail, before anything has changed (SPEC 15.10).
    pub fn change_plan<R>(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut Plan) -> R,
    ) -> Result<R, ApiError> {
        let plan = self.plans.by_id_mut(id)?;
        let Some(files) = &mut self.files else {
            return Ok(change(plan));
        };
        let mut changed = plan.clone();
        let result = change(&mut changed);
        files.save(plan, &changed).map_err(|e| unsaved(id, e))?;
        *plan = changed;
        Ok(result)
    }

    pub fn delete_plan(&mut self, id: &str) -> Result<(), ApiError> {
        self.plans.by_id(id)?;
        if let Some(files) = &mut self.files {
            files.delete(id).map_err(|e| unsaved(id, e))?;
        }
        self.plans.shift_remove(id);
        Ok(())
    }
}
