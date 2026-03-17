use std::time::Duration;

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

#[derive(Debug, Clone)]
pub struct ConvergeOptions {
    pub policy: ConvergePolicy,
    pub dry_run: bool,
    pub busy_timeout: Duration,
    pub max_retries: u32,
}

impl Default for ConvergeOptions {
    fn default() -> Self {
        Self {
            policy: ConvergePolicy::default(),
            dry_run: false,
            busy_timeout: Duration::from_secs(5),
            max_retries: 3,
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
    pub indexes_changed: usize,
    pub views_changed: usize,
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
            indexes_changed: 0,
            views_changed: 0,
            duration: Duration::ZERO,
            plan_sql: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
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
            || self.indexes_changed > 0
            || self.views_changed > 0
    }
}
