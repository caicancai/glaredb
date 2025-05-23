use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use glaredb_error::{DbError, Result};

use super::Catalog;
use super::create::{CreateSchemaInfo, CreateTableInfo, CreateViewInfo};
use super::drop::DropInfo;
use super::entry::CatalogEntry;
use super::memory::MemoryCatalog;
use crate::arrays::scalar::ScalarValue;
use crate::execution::operators::PlannedOperator;
use crate::execution::planner::OperatorIdGen;
use crate::storage::storage_manager::StorageManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    ReadWrite,
    ReadOnly,
}

impl AccessMode {
    pub fn as_str(&self) -> &str {
        match self {
            AccessMode::ReadWrite => "ReadWrite",
            AccessMode::ReadOnly => "ReadOnly",
        }
    }
}

impl fmt::Display for AccessMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachInfo {
    /// Options used for connecting to the database.
    ///
    /// This includes things like connection strings, and other possibly
    /// sensitive info.
    pub options: HashMap<String, ScalarValue>,
}

#[derive(Debug)]
pub struct Database {
    pub(crate) name: String,
    pub(crate) mode: AccessMode,
    // TODO: Allow other catalog types.
    pub(crate) catalog: Arc<MemoryCatalog>,
    // TODO: Allow other storage managers.
    pub(crate) storage: Arc<StorageManager>,
    pub(crate) attach_info: Option<AttachInfo>,
}

impl Database {
    pub fn plan_create_view(
        &self,
        id_gen: &mut OperatorIdGen,
        schema: &str,
        create: CreateViewInfo,
    ) -> Result<PlannedOperator> {
        self.check_can_write()?;
        self.catalog.plan_create_view(id_gen, schema, create)
    }

    pub fn plan_create_table(
        &self,
        id_gen: &mut OperatorIdGen,
        schema: &str,
        create: CreateTableInfo,
    ) -> Result<PlannedOperator> {
        self.check_can_write()?;
        self.catalog
            .plan_create_table(&self.storage, id_gen, schema, create)
    }

    pub fn plan_create_table_as(
        &self,
        id_gen: &mut OperatorIdGen,
        schema: &str,
        create: CreateTableInfo,
    ) -> Result<PlannedOperator> {
        self.check_can_write()?;
        self.catalog
            .plan_create_table_as(&self.storage, id_gen, schema, create)
    }

    pub fn plan_insert(
        &self,

        id_gen: &mut OperatorIdGen,
        table: Arc<CatalogEntry>,
    ) -> Result<PlannedOperator> {
        self.check_can_write()?;
        self.catalog.plan_insert(&self.storage, id_gen, table)
    }

    pub fn plan_create_schema(
        &self,

        id_gen: &mut OperatorIdGen,
        create: CreateSchemaInfo,
    ) -> Result<PlannedOperator> {
        self.check_can_write()?;
        self.catalog.plan_create_schema(id_gen, create)
    }

    pub fn plan_drop(&self, id_gen: &mut OperatorIdGen, drop: DropInfo) -> Result<PlannedOperator> {
        self.check_can_write()?;
        self.catalog.plan_drop(&self.storage, id_gen, drop)
    }

    fn check_can_write(&self) -> Result<()> {
        if self.mode != AccessMode::ReadWrite {
            return Err(DbError::new(format!(
                "Database '{}' is not writable",
                self.name
            )));
        }
        Ok(())
    }
}
