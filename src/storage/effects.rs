//! P18-T03: the durable record of external effects.
//!
//! `provider_calls` records *spend*: it answers "was this call allowed and what did it cost".
//! It cannot answer the question a stolen or restarted turn has to ask first: "did this effect
//! already happen out there?". That question needs a row written *before* the attempt and settled
//! after it, with a stable identity, which is what `external_effects` holds.
//!
//! The identity deliberately excludes the fence. A steal raises the fence, so a fence-bearing key
//! would be a different key for the same logical effect and the new holder would sail past
//! uniqueness into a second paid call. The fence is recorded so an operator can see which holder
//! attempted the effect; it does not identify it.

use super::{leases::Lease, now, uid, DbStore};
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

/// A reservation carries the exact lease that authorized it. Settlement must present this same
/// remembered fence; reading the current fence later would authorize a stale worker after takeover.
#[derive(Clone, Debug)]
pub struct ExternalEffectReservation {
    effect_id: String,
    lease: Lease,
}

#[cfg(test)]
impl ExternalEffectReservation {
    pub(crate) fn id(&self) -> &str {
        &self.effect_id
    }
}

/// How an attempted external effect ended. `Unknown` is not a failure: it means the effect may
/// have happened, which is the one outcome a human has to decide about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectOutcome {
    Succeeded,
    /// A known non-effect: the attempt provably did not reach the outside world. No call site
    /// can prove that yet -- a send that errors may still have been received -- so today only the
    /// storage tests construct it. P18-T04 settles pre-dispatch refusals here once a refused
    /// fence check can prove the effect never left.
    #[allow(dead_code)]
    Failed,
    Unknown,
}

impl EffectOutcome {
    fn state(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

impl DbStore {
    /// Record an external effect before it is attempted.
    ///
    /// Returns `None` when the effect cannot be attributed to a recorded turn: the row references
    /// `chat_receipts`, and inserting it for an unrecorded request would fail the foreign key and
    /// turn a bookkeeping gap into a refused provider call. Those paths are not recorded yet, and
    /// this returns `None` rather than pretending they are.
    ///
    /// An identity that is already reserved, succeeded, or unknown is refused: re-attempting it is
    /// exactly the duplicate this table exists to prevent.
    pub async fn reserve_external_effect(
        &self,
        request_id: String,
        step_identity: String,
        payload_digest: String,
        kind: String,
        lease: Option<Lease>,
    ) -> Result<Option<ExternalEffectReservation>> {
        let effect_id = uid();
        let returned = effect_id.clone();
        let key = format!("{request_id}:{step_identity}:{payload_digest}");
        let presented_lease = lease.clone();
        let recorded = self
            .run(move |c| {
                let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let turn_is_recorded = tx
                    .query_row(
                        "SELECT 1 FROM chat_receipts WHERE request_id=?1",
                        [&request_id],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()?
                    .is_some();
                if !turn_is_recorded {
                    return Ok(false);
                }
                let state: String = tx.query_row(
                    "SELECT state FROM chat_receipts WHERE request_id=?1",
                    [&request_id],
                    |r| r.get(0),
                )?;
                if state == "generating" {
                    let presented = presented_lease.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "provider effect on generating turn {request_id} has no remembered lease"
                        )
                    })?;
                    if presented.request_id != request_id {
                        bail!("provider effect lease belongs to a different turn");
                    }
                    super::leases::guard_fence(&tx, presented)?;
                } else {
                    // Post-turn extraction and maintenance are not turn execution and have no live
                    // lease to fence. They remain outside the turn-effect ledger.
                    return Ok(false);
                }
                let existing = tx
                    .query_row(
                        "SELECT effect_id,state FROM external_effects WHERE idempotency_key=?1",
                        [&key],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                    )
                    .optional()?;
                if let Some((prior, state)) = existing {
                    match state.as_str() {
                        "succeeded" => bail!(
                            "external effect {prior} already succeeded; refusing to repeat it"
                        ),
                        "reserved" => {
                            bail!("external effect {prior} is already in flight")
                        }
                        "unknown" => bail!(
                            "external effect {prior} has an unknown outcome and needs a human decision"
                        ),
                        _ => bail!(
                            "external effect {prior} already failed under this identity; a retry needs a fresh step identity"
                        ),
                    }
                }
                tx.execute(
                    "INSERT INTO external_effects(effect_id,request_id,step_identity,payload_digest,idempotency_key,kind,fence,state,attempted_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'reserved',?8)",
                    params![
                        effect_id,
                        request_id,
                        step_identity,
                        payload_digest,
                        key,
                        kind,
                        presented_lease.as_ref().expect("generating effects require a lease").fence,
                        now()
                    ],
                )?;
                tx.commit()?;
                Ok(true)
            })
            .await?;
        Ok(recorded.then(|| ExternalEffectReservation {
            effect_id: returned,
            lease: lease.expect("a recorded effect was guarded by a lease"),
        }))
    }

    /// Settle a reserved effect exactly once. An `Unknown` outcome must carry a reason, because
    /// `unknown` with no reason is an alarm with nothing for a human to act on.
    pub async fn settle_external_effect(
        &self,
        effect: ExternalEffectReservation,
        outcome: EffectOutcome,
        outcome_ref: Option<String>,
        reason: Option<String>,
    ) -> Result<()> {
        if outcome == EffectOutcome::Unknown && reason.is_none() {
            bail!("an unknown external effect outcome must carry a reason");
        }
        let state = outcome.state();
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            super::leases::guard_fence(&tx, &effect.lease)?;
            if tx.execute(
                "UPDATE external_effects SET state=?2,outcome_ref=?3,reason=?4,settled_at=?5 WHERE effect_id=?1 AND state='reserved'",
                params![effect.effect_id, state, outcome_ref, reason, now()],
            )? != 1
            {
                bail!("external effect reservation is not active");
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Effects whose outcome is unknown and therefore owe a human a decision. This is the read a
    /// surfacing path consumes; it makes no decision itself.
    /// Not yet read by a running surface: decision 4 settled *that* unknown outcomes go to a
    /// human, and P18-T05 builds the surface that shows them. Kept and tested here so the read
    /// the surface needs is already proven against the schema.
    #[allow(dead_code)]
    pub async fn unknown_external_effects(&self, limit: i64) -> Result<Vec<Value>> {
        self.read(move |c| {
            let mut stmt = c.prepare(
                "SELECT effect_id,request_id,step_identity,kind,fence,reason,attempted_at,settled_at FROM external_effects WHERE state='unknown' ORDER BY attempted_at LIMIT ?1",
            )?;
            let rows = stmt
                .query_map([limit], |r| {
                    Ok(json!({
                        "effect_id": r.get::<_, String>(0)?,
                        "request_id": r.get::<_, String>(1)?,
                        "step_identity": r.get::<_, String>(2)?,
                        "kind": r.get::<_, String>(3)?,
                        "fence": r.get::<_, i64>(4)?,
                        "reason": r.get::<_, Option<String>>(5)?,
                        "attempted_at": r.get::<_, String>(6)?,
                        "settled_at": r.get::<_, Option<String>>(7)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }
}
