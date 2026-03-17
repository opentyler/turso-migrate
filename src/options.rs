use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnRenameHint {
    pub table: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataMigration {
    pub id: String,
    pub statements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DestructiveChangeSet {
    pub tables_to_drop: Vec<String>,
    pub columns_to_drop: Vec<(String, String)>,
    pub tables_to_rebuild: Vec<String>,
}

impl DestructiveChangeSet {
    pub fn has_changes(&self) -> bool {
        !self.tables_to_drop.is_empty()
            || !self.columns_to_drop.is_empty()
            || !self.tables_to_rebuild.is_empty()
    }

    pub fn blocked_operations(&self) -> Vec<String> {
        let mut blocked = Vec::new();
        blocked.extend(
            self.tables_to_drop
                .iter()
                .map(|t| format!("DROP TABLE {t}")),
        );
        blocked.extend(
            self.columns_to_drop
                .iter()
                .map(|(t, c)| format!("DROP COLUMN {t}.{c}")),
        );
        blocked.extend(
            self.tables_to_rebuild
                .iter()
                .map(|t| format!("REBUILD TABLE {t}")),
        );
        blocked
    }
}

pub type PreDestructiveHook =
    Arc<dyn Fn(&DestructiveChangeSet) -> Result<(), String> + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failpoint {
    BeforeIntrospect,
    BeforeExecute,
    AfterExecuteBeforeState,
}

impl Failpoint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeforeIntrospect => "before_introspect",
            Self::BeforeExecute => "before_execute",
            Self::AfterExecuteBeforeState => "after_execute_before_state",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConvergePolicy {
    pub allow_table_drops: bool,
    pub allow_column_drops: bool,
    pub allow_table_rebuilds: bool,
    pub max_tables_affected: Option<usize>,
}

impl Default for ConvergePolicy {
    fn default() -> Self {
        Self {
            allow_table_drops: false,
            allow_column_drops: false,
            allow_table_rebuilds: true,
            max_tables_affected: None,
        }
    }
}

impl ConvergePolicy {
    pub fn permissive() -> Self {
        Self {
            allow_table_drops: true,
            allow_column_drops: true,
            allow_table_rebuilds: true,
            max_tables_affected: None,
        }
    }
}

#[derive(Clone)]
pub struct ConvergeOptions {
    pub policy: ConvergePolicy,
    pub dry_run: bool,
    pub busy_timeout: Duration,
    pub max_retries: u32,
    pub backup_before_destructive: Option<PathBuf>,
    pub data_migrations: Vec<DataMigration>,
    pub rename_hints: Vec<ColumnRenameHint>,
    pub pre_destructive_hook: Option<PreDestructiveHook>,
    pub failpoint: Option<Failpoint>,
}

impl fmt::Debug for ConvergeOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConvergeOptions")
            .field("policy", &self.policy)
            .field("dry_run", &self.dry_run)
            .field("busy_timeout", &self.busy_timeout)
            .field("max_retries", &self.max_retries)
            .field("backup_before_destructive", &self.backup_before_destructive)
            .field("data_migrations", &self.data_migrations)
            .field("rename_hints", &self.rename_hints)
            .field(
                "pre_destructive_hook",
                &self.pre_destructive_hook.as_ref().map(|_| "<hook>"),
            )
            .field("failpoint", &self.failpoint)
            .finish()
    }
}

impl Default for ConvergeOptions {
    fn default() -> Self {
        Self {
            policy: ConvergePolicy::default(),
            dry_run: false,
            busy_timeout: Duration::from_secs(5),
            max_retries: 3,
            backup_before_destructive: None,
            data_migrations: Vec::new(),
            rename_hints: Vec::new(),
            pre_destructive_hook: None,
            failpoint: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConvergeReport {
    pub mode: ConvergeMode,
    pub tables_created: usize,
    pub tables_rebuilt: usize,
    pub tables_dropped: usize,
    pub columns_added: usize,
    pub columns_dropped: usize,
    pub columns_renamed: usize,
    pub indexes_changed: usize,
    pub views_changed: usize,
    pub data_migrations_applied: usize,
    pub duration: Duration,
    pub plan_sql: Vec<String>,
}

impl Default for ConvergeReport {
    fn default() -> Self {
        Self {
            mode: ConvergeMode::FastPath,
            tables_created: 0,
            tables_rebuilt: 0,
            tables_dropped: 0,
            columns_added: 0,
            columns_dropped: 0,
            columns_renamed: 0,
            indexes_changed: 0,
            views_changed: 0,
            data_migrations_applied: 0,
            duration: Duration::ZERO,
            plan_sql: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvergeMode {
    FastPath,
    SlowPath,
    CrashRecovery,
    NoOp,
    DryRun,
}

impl ConvergeReport {
    pub fn fast_path(duration: Duration) -> Self {
        Self {
            mode: ConvergeMode::FastPath,
            duration,
            ..Default::default()
        }
    }

    pub fn had_changes(&self) -> bool {
        self.tables_created > 0
            || self.tables_rebuilt > 0
            || self.tables_dropped > 0
            || self.columns_added > 0
            || self.columns_dropped > 0
            || self.columns_renamed > 0
            || self.indexes_changed > 0
            || self.views_changed > 0
            || self.data_migrations_applied > 0
    }
}
