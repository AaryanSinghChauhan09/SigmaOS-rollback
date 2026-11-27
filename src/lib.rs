//! SigmaOS rollback component.
//!
//! Transactional OS state journal: callers record the changes a
//! transaction made, and `rollback_last` / `rollback_to_generation`
//! yield the inverse steps so transactions can be atomically undone.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::VecDeque;

use thiserror::Error;

/// Error types for this component.
#[derive(Debug, Error)]
pub enum ComponentError {
    /// Not initialised.
    #[error("component not initialised")]
    NotInitialised,
    /// Unsupported operation.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// No transaction to roll back.
    #[error("no transaction to roll back")]
    NothingToRollBack,
    /// Transaction id already in use.
    #[error("duplicate transaction id: {0}")]
    DuplicateId(String),
    /// Transaction id not found in journal.
    #[error("unknown transaction id: {0}")]
    UnknownId(String),
}

/// Result alias.
pub type ComponentResult<T> = Result<T, ComponentError>;

/// The kind of change a transaction step performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Installed something (package, file set).
    Install,
    /// Removed something.
    Remove,
    /// Modified a file; `backup` holds the pre-change copy location.
    Modify {
        /// Path to the saved backup of the original content.
        backup: String,
    },
}

/// One recorded change inside a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// What was done.
    pub action: Action,
    /// What it was done to (package name, file path, ...).
    pub target: String,
}

/// The inverse of a step, returned by rollback planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InverseStep {
    /// Action needed to undo the original step.
    pub action: Action,
    /// Target the inverse applies to.
    pub target: String,
}

/// A named group of changes applied together.
#[derive(Debug, Clone)]
pub struct Transaction {
    /// Caller-chosen identifier.
    pub id: String,
    /// Steps applied, in order.
    pub steps: Vec<Step>,
    /// Whether this transaction has been rolled back.
    pub rolled_back: bool,
}

/// Bounded journal of transactions with rollback planning.
#[derive(Debug, Default)]
pub struct RollbackManager {
    journal: VecDeque<Transaction>,
    capacity: usize,
    next_id: u64,
}

impl RollbackManager {
    /// Create a manager keeping at most `capacity` transactions.
    pub fn new(capacity: usize) -> Self {
        Self {
            journal: VecDeque::new(),
            capacity: capacity.max(1),
            next_id: 1,
        }
    }

    /// Record a completed transaction.
    pub fn record(&mut self, id: impl Into<String>, steps: Vec<Step>) -> ComponentResult<()> {
        let id = id.into();
        if self.journal.iter().any(|t| t.id == id) {
            return Err(ComponentError::DuplicateId(id));
        }
        if self.journal.len() == self.capacity {
            self.journal.pop_front();
        }
        self.journal.push_back(Transaction {
            id,
            steps,
            rolled_back: false,
        });
        Ok(())
    }

    /// Generate an id for a new transaction (`txn-N`).
    pub fn next_id(&mut self) -> String {
        let id = format!("txn-{}", self.next_id);
        self.next_id += 1;
        id
    }

    fn invert_steps(steps: &[Step]) -> Vec<InverseStep> {
        steps
            .iter()
            .rev()
            .map(|s| InverseStep {
                action: match &s.action {
                    Action::Install => Action::Remove,
                    Action::Remove => Action::Install,
                    Action::Modify { backup } => Action::Modify {
                        backup: backup.clone(),
                    },
                },
                target: s.target.clone(),
            })
            .collect()
    }

    /// Plan the rollback of the most recent non-rolled-back transaction.
    ///
    /// Returns the inverse steps in reverse order; the caller performs
    /// them, then calls [`mark_rolled_back`](Self::mark_rolled_back).
    pub fn rollback_last(&mut self) -> ComponentResult<Vec<InverseStep>> {
        let txn = self
            .journal
            .iter_mut()
            .rev()
            .find(|t| !t.rolled_back)
            .ok_or(ComponentError::NothingToRollBack)?;
        let plan = Self::invert_steps(&txn.steps);
        txn.rolled_back = true;
        Ok(plan)
    }

    /// Plan atomic rollback of all active transactions recorded *after* `target_id`.
    ///
    /// Returns the combined inverse steps in reverse chronological order.
    pub fn rollback_to_generation(&mut self, target_id: &str) -> ComponentResult<Vec<InverseStep>> {
        let target_idx = self
            .journal
            .iter()
            .position(|t| t.id == target_id)
            .ok_or_else(|| ComponentError::UnknownId(target_id.to_string()))?;

        let mut plan = Vec::new();
        for txn in self.journal.iter_mut().skip(target_idx + 1).rev() {
            if !txn.rolled_back {
                plan.extend(Self::invert_steps(&txn.steps));
                txn.rolled_back = true;
            }
        }
        if plan.is_empty() {
            return Err(ComponentError::NothingToRollBack);
        }
        Ok(plan)
    }

    /// Mark the transaction with `id` as rolled back without producing a
    /// plan (e.g. rollback was performed by an external tool).
    pub fn mark_rolled_back(&mut self, id: &str) -> ComponentResult<()> {
        let txn = self
            .journal
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| ComponentError::UnknownId(id.to_string()))?;
        txn.rolled_back = true;
        Ok(())
    }

    /// All transactions, oldest first.
    pub fn history(&self) -> std::collections::vec_deque::Iter<'_, Transaction> {
        self.journal.iter()
    }

    /// Number of transactions retained.
    pub fn len(&self) -> usize {
        self.journal.len()
    }

    /// Whether the journal is empty.
    pub fn is_empty(&self) -> bool {
        self.journal.is_empty()
    }
}

/// Component handle.
#[derive(Debug, Default, Clone)]
pub struct Component {
    initialised: bool,
}

impl Component {
    /// Create uninitialised component.
    pub fn new() -> Self {
        Self::default()
    }
    /// Initialise.
    pub fn init(&mut self) -> ComponentResult<()> {
        self.initialised = true;
        Ok(())
    }
    /// Is ready?
    pub fn is_ready(&self) -> bool {
        self.initialised
    }
    /// Run.
    pub fn run(&self) -> ComponentResult<()> {
        if !self.initialised {
            return Err(ComponentError::NotInitialised);
        }
        Ok(())
    }
    /// Reset.
    pub fn reset(&mut self) {
        self.initialised = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_not_ready() {
        assert!(!Component::new().is_ready());
    }
    #[test]
    fn test_init_ok() {
        let mut c = Component::new();
        c.init().unwrap();
        assert!(c.is_ready());
    }
    #[test]
    fn test_run_before_init_err() {
        assert!(Component::new().run().is_err());
    }
    #[test]
    fn test_run_after_init_ok() {
        let mut c = Component::new();
        c.init().unwrap();
        assert!(c.run().is_ok());
    }
    #[test]
    fn test_reset() {
        let mut c = Component::new();
        c.init().unwrap();
        c.reset();
        assert!(!c.is_ready());
    }

    #[test]
    fn rollback_inverts_steps_in_reverse() {
        let mut m = RollbackManager::new(8);
        m.record(
            "t1",
            vec![
                Step {
                    action: Action::Install,
                    target: "firefox".into(),
                },
                Step {
                    action: Action::Modify {
                        backup: "/bak/hosts".into(),
                    },
                    target: "/etc/hosts".into(),
                },
            ],
        )
        .unwrap();
        let plan = m.rollback_last().unwrap();
        assert_eq!(plan.len(), 2);
        // reversed order, inverse actions
        assert_eq!(plan[0].target, "/etc/hosts");
        assert_eq!(
            plan[0].action,
            Action::Modify {
                backup: "/bak/hosts".into()
            }
        );
        assert_eq!(plan[1].action, Action::Remove);
        assert_eq!(plan[1].target, "firefox");
        // second call finds nothing new
        assert!(matches!(
            m.rollback_last(),
            Err(ComponentError::NothingToRollBack)
        ));
    }

    #[test]
    fn rollback_skips_already_rolled_back() {
        let mut m = RollbackManager::new(8);
        m.record(
            "a",
            vec![Step {
                action: Action::Install,
                target: "x".into(),
            }],
        )
        .unwrap();
        m.record(
            "b",
            vec![Step {
                action: Action::Remove,
                target: "y".into(),
            }],
        )
        .unwrap();
        m.mark_rolled_back("b").unwrap();
        let plan = m.rollback_last().unwrap();
        assert_eq!(plan[0].target, "x");
        assert_eq!(plan[0].action, Action::Remove);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let mut m = RollbackManager::new(2);
        for i in 0..3 {
            m.record(format!("t{i}"), vec![]).unwrap();
        }
        assert_eq!(m.len(), 2);
        assert_eq!(m.history().next().unwrap().id, "t1");
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut m = RollbackManager::new(8);
        m.record("x", vec![]).unwrap();
        assert!(matches!(
            m.record("x", vec![]),
            Err(ComponentError::DuplicateId(_))
        ));
    }

    #[test]
    fn test_unknown_id_rejected_aaa() {
        // Arrange
        let mut m = RollbackManager::new(8);
        // Act
        let res = m.mark_rolled_back("missing");
        // Assert
        assert!(matches!(res, Err(ComponentError::UnknownId(_))));
    }

    #[test]
    fn test_rollback_to_generation_aaa() {
        // Arrange
        let mut m = RollbackManager::new(8);
        m.record("gen-1", vec![Step { action: Action::Install, target: "base".into() }]).unwrap();
        m.record("gen-2", vec![Step { action: Action::Install, target: "pkg-a".into() }]).unwrap();
        m.record("gen-3", vec![Step { action: Action::Remove, target: "pkg-b".into() }]).unwrap();
        // Act
        let plan = m.rollback_to_generation("gen-1").unwrap();
        // Assert
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], InverseStep { action: Action::Install, target: "pkg-b".into() });
        assert_eq!(plan[1], InverseStep { action: Action::Remove, target: "pkg-a".into() });
    }

    #[test]
    fn test_rollback_to_generation_nothing_pending_aaa() {
        // Arrange
        let mut m = RollbackManager::new(8);
        m.record("gen-1", vec![Step { action: Action::Install, target: "base".into() }]).unwrap();
        // Act
        let res = m.rollback_to_generation("gen-1");
        // Assert
        assert!(matches!(res, Err(ComponentError::NothingToRollBack)));
    }

    #[test]
    fn test_capacity_evicts_oldest_generation_aaa() {
        // Arrange: cap=2, record 3 gens
        let mut m = RollbackManager::new(2);
        m.record("g1", vec![Step { action: Action::Install, target: "pkg-a".into() }]).unwrap();
        m.record("g2", vec![Step { action: Action::Install, target: "pkg-b".into() }]).unwrap();
        // Act: g1 should be evicted on g3 record
        m.record("g3", vec![Step { action: Action::Install, target: "pkg-c".into() }]).unwrap();
        // Assert: g1 is gone, g2 and g3 are present
        assert!(matches!(
            m.rollback_to_generation("g1"),
            Err(ComponentError::UnknownId(_))
        ));
        // g3→g2 rollback should succeed
        let plan = m.rollback_to_generation("g2").unwrap();
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn test_modify_step_inverts_to_modify_aaa() {
        // Arrange: a Modify step should invert to itself (restore backup)
        let mut m = RollbackManager::new(8);
        m.record("g1", vec![]).unwrap();
        m.record("g2", vec![Step {
            action: Action::Modify { backup: "/sigma/backups/etc-hosts".into() },
            target: "/etc/hosts".into(),
        }]).unwrap();
        // Act
        let plan = m.rollback_to_generation("g1").unwrap();
        // Assert
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].target, "/etc/hosts");
    }
}
