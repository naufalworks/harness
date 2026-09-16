//! P18-T04: worker identity, lease acquisition, heartbeat renewal, release, takeover of a lapsed
//! lease, and the fence check a durable write has to pass.
//!
//! What a lease is for: `chat_receipts.state='generating'` says a turn is being worked on, but not
//! *by whom*, and not *whether that worker is still alive*. With one worker those questions have
//! the same answer forever. With two, a worker that stalls past its expiry and then wakes up still
//! believes it owns the turn, and its writes are indistinguishable from the new holder's.
//!
//! What stops that is not the TTL. A TTL only decides when someone else may take over; it cannot
//! reach into the stalled worker and stop it. The fence does: the lease mints a strictly increasing
//! number, the holder remembers the number it acquired, and every durable write re-checks that
//! remembered number against the stored one *in the same transaction as the write*. A holder whose
//! lease was taken over now carries a number the database has moved past, so its write is refused
//! instead of merged. Checking before opening the transaction would leave the same race one layer
//! down, because the lease can lapse between the check and the write.
//!
//! Times here are issued by SQLite, never by the worker's clock. Two workers with skewed clocks
//! would otherwise disagree about whether a lease had expired, and the one running fast could
//! manufacture ownership by declaring the other's lease lapsed. `expires_at` is computed from the
//! database's own `now`, and expiry is judged by comparing against it, so skew between workers
//! cannot extend or revoke ownership.
//!
//! Takeover is separated from acquisition on purpose. `acquire_in_tx` never takes a turn away from
//! another worker; `steal_in_tx` is the only path that does, and it is refused unless the lease has
//! actually lapsed. The dangerous case is not the lease at all but the outside world: a holder that
//! died between reserving an external effect and settling it leaves an effect nobody can classify.
//! A takeover there would ask the new holder to redo work that may already have happened, so the
//! reservation is swept to `unknown` and the steal is refused. The turn then waits for a human,
//! which is the intended outcome rather than a gap.

use super::DbStore;
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};

/// How long a lease is valid without a heartbeat. Short enough that a dead worker's turn becomes
/// recoverable soon, long enough that an ordinary GC pause or slow provider call does not cost a
/// live worker its lease. Renewal must happen well inside this window; a worker that cannot renew
/// within the TTL is, by definition, not making progress the database can see.
pub const LEASE_TTL_SECONDS: i64 = 30;

/// SQLite-issued UTC timestamp. Every lease column uses this one expression so the schema's
/// `renewed_at >= acquired_at` and `expires_at > renewed_at` checks compare like with like.
const DB_NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

fn db_now_plus_ttl() -> String {
    format!("strftime('%Y-%m-%dT%H:%M:%fZ','now','+{LEASE_TTL_SECONDS} seconds')")
}

/// Identity of one running worker process.
///
/// A restarted process must never be able to present the previous run's identity: the point of a
/// lease is to distinguish "the worker that claimed this turn" from "a worker that looks like it".
/// Process ids recycle and a hostname is shared, so the identity carries a per-start nonce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerIdentity(String);

impl WorkerIdentity {
    pub fn for_this_process() -> Self {
        let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into());
        let nonce = super::uid();
        let nonce = nonce.split('-').next().unwrap_or("0").to_string();
        // The column caps at 128 characters; host:pid:nonce stays far inside that.
        Self(format!("{host}:{}:{nonce}", std::process::id()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A held lease. The fence, not the worker id, is what authorizes a write; the id is carried so a
/// refusal can name who actually holds the turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub request_id: String,
    pub worker_id: String,
    pub fence: i64,
}

/// Why an acquisition did not happen. `HeldByAnother` is not an error: a worker that finds a turn
/// already owned should move on, and only `steal_in_tx` may take a lapsed lease away from its
/// holder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcquireRefusal {
    UnknownRequest,
    HeldByAnother { worker_id: String, lapsed: bool },
}

/// Why a takeover did not happen.
///
/// `EffectsNeedDecision` is the important one: the previous holder died with an external effect
/// reserved and unsettled, so nobody knows whether that effect reached the outside world. Handing
/// the turn to a new worker would ask it to redo work that may already have happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StealRefusal {
    NoLease,
    /// The lease has not lapsed. Taking a live worker's turn is not recovery, it is a race.
    StillLive {
        worker_id: String,
    },
    /// The caller already holds it; `acquire_in_tx` is the path for that.
    AlreadyHeld,
    /// Swept to `unknown` by this call. A human decides; nothing is retried automatically.
    EffectsNeedDecision {
        effect_ids: Vec<String>,
    },
}

/// Take a lapsed lease away from the worker that held it.
///
/// A takeover is only safe when the outside world is in a known state. The previous holder may
/// have died between reserving an external effect and settling it, and in that case the database
/// cannot say whether the effect happened. Rather than hand the turn to a new worker that would
/// redo it, those reservations are swept to `unknown` -- carrying the reason a human will read --
/// and the steal is refused. This is decision 4 applied literally: an effect with an unknown
/// outcome is surfaced for a decision, never auto-retried.
///
/// The sweep is a *write*, and it happens on the refusal path. The caller must commit this
/// transaction rather than roll it back on refusal, because the evidence a human needs is exactly
/// what was just written.
pub fn steal_in_tx(
    tx: &Transaction<'_>,
    request_id: &str,
    worker_id: &str,
) -> Result<std::result::Result<Lease, StealRefusal>> {
    let existing: Option<(String, i64, String, bool)> = tx
        .query_row(
            &format!(
                "SELECT worker_id,fence,state,expires_at > {DB_NOW} FROM worker_leases WHERE request_id=?1"
            ),
            [request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((holder, prior, state, unexpired)) = existing else {
        return Ok(Err(StealRefusal::NoLease));
    };
    if holder == worker_id {
        return Ok(Err(StealRefusal::AlreadyHeld));
    }
    if state == "held" && unexpired {
        return Ok(Err(StealRefusal::StillLive { worker_id: holder }));
    }
    let in_flight: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT effect_id FROM external_effects WHERE request_id=?1 AND state='reserved' ORDER BY attempted_at",
        )?;
        let rows = stmt
            .query_map([request_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    if !in_flight.is_empty() {
        // Settled here rather than by the async settle path because the decision and its evidence
        // must be one atomic act: a sweep that committed without the refusal, or a refusal without
        // the sweep, would each leave the operator a different lie.
        tx.execute(
            "UPDATE external_effects SET state='unknown',reason=?2,settled_at=?3 WHERE request_id=?1 AND state='reserved'",
            params![
                request_id,
                "lease_lapsed_with_effect_in_flight",
                super::now()
            ],
        )?;
        return Ok(Err(StealRefusal::EffectsNeedDecision {
            effect_ids: in_flight,
        }));
    }
    let next = prior + 1;
    tx.execute(
        &format!(
            "UPDATE worker_leases SET worker_id=?2,fence=?3,acquired_at={DB_NOW},renewed_at={DB_NOW},expires_at={ttl},state='held' WHERE request_id=?1",
            ttl = db_now_plus_ttl()
        ),
        params![request_id, worker_id, next],
    )?;
    Ok(Ok(Lease {
        request_id: request_id.to_string(),
        worker_id: worker_id.to_string(),
        fence: next,
    }))
}

/// Take the lease on a turn inside an existing transaction.
///
/// This is the in-transaction form because the caller that matters -- claiming a turn -- must take
/// ownership in the same transaction that moves the receipt to `generating`. A claim that committed
/// without a lease, or a lease that committed without a claim, would leave a turn whose owner and
/// whose state disagree.
///
/// Re-acquiring a turn this same worker already holds mints a *higher* fence rather than reusing
/// the old one. The previous fence may still be in flight inside a write this process started
/// before it lost track of its own lease; raising the fence invalidates that write instead of
/// leaving two live authorizations.
pub fn acquire_in_tx(
    tx: &Transaction<'_>,
    request_id: &str,
    worker_id: &str,
) -> Result<std::result::Result<Lease, AcquireRefusal>> {
    let turn_is_recorded = tx
        .query_row(
            "SELECT 1 FROM chat_receipts WHERE request_id=?1",
            [request_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    if !turn_is_recorded {
        // The lease row references chat_receipts. Inserting one for an unrecorded request would
        // fail the foreign key; name the cause instead of surfacing a constraint error.
        return Ok(Err(AcquireRefusal::UnknownRequest));
    }
    let existing: Option<(String, i64, String, bool)> = tx
        .query_row(
            &format!(
                "SELECT worker_id,fence,state,expires_at > {DB_NOW} FROM worker_leases WHERE request_id=?1"
            ),
            [request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let fence = match existing {
        None => {
            tx.execute(
                &format!(
                    "INSERT INTO worker_leases(request_id,worker_id,fence,acquired_at,renewed_at,expires_at,state) VALUES(?1,?2,1,{DB_NOW},{DB_NOW},{ttl},'held')",
                    ttl = db_now_plus_ttl()
                ),
                params![request_id, worker_id],
            )?;
            1
        }
        Some((holder, _, state, unexpired)) if holder != worker_id && state == "held" => {
            // Unexpired: someone else is working on it. Lapsed: taking it is a steal, which this
            // slice does not implement, because the new holder would first have to establish which
            // external effects the old one had already attempted.
            return Ok(Err(AcquireRefusal::HeldByAnother {
                worker_id: holder,
                lapsed: !unexpired,
            }));
        }
        Some((_, prior, _, _)) => {
            let next = prior + 1;
            tx.execute(
                &format!(
                    "UPDATE worker_leases SET worker_id=?2,fence=?3,acquired_at={DB_NOW},renewed_at={DB_NOW},expires_at={ttl},state='held' WHERE request_id=?1",
                    ttl = db_now_plus_ttl()
                ),
                params![request_id, worker_id, next],
            )?;
            next
        }
    };
    Ok(Ok(Lease {
        request_id: request_id.to_string(),
        worker_id: worker_id.to_string(),
        fence,
    }))
}

/// Refuse a durable write whose fence is no longer current.
///
/// Deliberately a plain function taking the caller's `Transaction`, not a method on the store: it
/// has to run inside the *same* transaction as the write it authorizes. A separate "check, then
/// write" pair reopens the race it exists to close.
///
/// The error text names the current holder, because the operator question after a refused write is
/// always "then who owns this turn".
pub fn guard_fence(tx: &Transaction<'_>, lease: &Lease) -> Result<()> {
    let row: Option<(String, i64, String, bool)> = tx
        .query_row(
            &format!(
                "SELECT worker_id,fence,state,expires_at > {DB_NOW} FROM worker_leases WHERE request_id=?1"
            ),
            [&lease.request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((holder, fence, state, unexpired)) = row else {
        bail!(
            "no lease on {}: refusing a durable write at fence {}",
            lease.request_id,
            lease.fence
        );
    };
    if holder != lease.worker_id || fence != lease.fence {
        bail!(
            "lease on {} is held by {holder} at fence {fence}: refusing a durable write from {} at fence {}",
            lease.request_id,
            lease.worker_id,
            lease.fence
        );
    }
    if state != "held" {
        bail!(
            "lease on {} is {state}: refusing a durable write at fence {}",
            lease.request_id,
            lease.fence
        );
    }
    if !unexpired {
        // Expiry is judged by the database's clock, so a stalled worker cannot talk itself into
        // still being current. This is the case the TTL alone could not stop.
        bail!(
            "lease on {} lapsed at fence {}: refusing a durable write",
            lease.request_id,
            lease.fence
        );
    }
    Ok(())
}

impl DbStore {
    /// This process's worker identity.
    pub fn worker_identity(&self) -> &WorkerIdentity {
        &self.worker
    }

    /// Record that this process holds a lease, so later writes on the same turn can present the
    /// fence they acquired rather than whatever the row happens to say at write time. Reading the
    /// current fence at write time would authorize exactly the stale writer this guards against.
    pub fn remember_lease(&self, lease: Lease) {
        if let Ok(mut held) = self.held_leases.lock() {
            held.insert(lease.request_id.clone(), lease);
        }
    }

    /// The lease this process acquired for a turn, if any. `None` means this process never claimed
    /// the turn: HTTP-initiated writes such as cancellation and the restart recovery sweep are not
    /// lease holders and are not treated as one.
    pub fn held_lease(&self, request_id: &str) -> Option<Lease> {
        self.held_leases
            .lock()
            .ok()
            .and_then(|held| held.get(request_id).cloned())
    }

    /// Forget a lease after releasing it. Kept separate from `release_lease` so a failed release
    /// does not silently drop the fence a later write would still need to present.
    pub fn forget_lease(&self, request_id: &str) {
        if let Ok(mut held) = self.held_leases.lock() {
            held.remove(request_id);
        }
    }

    /// Extend a held lease. Returns `false` when the lease is gone, lapsed, or no longer this
    /// worker's at this fence -- the signal to stop working on the turn, not to retry. A heartbeat
    /// that re-acquired a lapsed lease would defeat expiry entirely.
    pub async fn renew_lease(&self, lease: &Lease) -> Result<bool> {
        let (request_id, worker_id, fence) = (
            lease.request_id.clone(),
            lease.worker_id.clone(),
            lease.fence,
        );
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let renewed = tx.execute(
                &format!(
                    "UPDATE worker_leases SET renewed_at={DB_NOW},expires_at={ttl} WHERE request_id=?1 AND worker_id=?2 AND fence=?3 AND state='held' AND expires_at > {DB_NOW}",
                    ttl = db_now_plus_ttl()
                ),
                params![request_id, worker_id, fence],
            )?;
            tx.commit()?;
            Ok(renewed == 1)
        })
        .await
    }

    /// Give the lease up. The row stays and keeps its fence: deleting it would let the next
    /// acquisition restart at 1, re-authorizing a pre-crash writer that still carries the old
    /// number. Releasing a lease this worker no longer holds is a no-op, not an error.
    pub async fn release_lease(&self, lease: &Lease) -> Result<bool> {
        let (request_id, worker_id, fence) = (
            lease.request_id.clone(),
            lease.worker_id.clone(),
            lease.fence,
        );
        let forget = lease.request_id.clone();
        let released = self
            .run(move |c| {
                let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let released = tx.execute(
                    "UPDATE worker_leases SET state='released' WHERE request_id=?1 AND worker_id=?2 AND fence=?3 AND state='held'",
                    params![request_id, worker_id, fence],
                )?;
                tx.commit()?;
                Ok(released == 1)
            })
            .await?;
        self.forget_lease(&forget);
        Ok(released)
    }

    /// Release whatever lease this process holds on a finished turn, if any.
    ///
    /// A failed release is logged rather than propagated: the turn is already durably terminal, so
    /// turning a bookkeeping failure into a turn failure would be a worse outcome. The lease then
    /// simply expires, which is the same path a crashed worker takes.
    pub async fn release_held_lease(&self, request_id: &str) {
        let Some(lease) = self.held_lease(request_id) else {
            return;
        };
        if self.release_lease(&lease).await.is_err() {
            eprintln!("{{\"event\":\"lease_release_failed\"}}");
            self.forget_lease(request_id);
        }
    }
}
