use glaredb_error::Result;

use super::binder::bind_context::BindContext;
use super::binder::table_list::TableRef;
use super::operator::{LogicalNode, Node};
use crate::explain::explainable::{EntryBuilder, ExplainConfig, ExplainEntry, Explainable};
use crate::expr::Expression;

/// Emit no rows with some number of columns.
///
/// This is used when we detect a filter is always false. Instead of executing
/// the child plans, we replace it with this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalNoRows {
    pub table_refs: Vec<TableRef>,
}

impl LogicalNode for Node<LogicalNoRows> {
    fn name(&self) -> &'static str {
        "NoRows"
    }

    fn get_output_table_refs(&self, _bind_context: &BindContext) -> Vec<TableRef> {
        self.node.table_refs.clone()
    }

    fn for_each_expr<'a, F>(&'a self, _func: F) -> Result<()>
    where
        F: FnMut(&'a Expression) -> Result<()>,
    {
        Ok(())
    }

    fn for_each_expr_mut<'a, F>(&'a mut self, _func: F) -> Result<()>
    where
        F: FnMut(&'a mut Expression) -> Result<()>,
    {
        Ok(())
    }
}

impl Explainable for LogicalNoRows {
    fn explain_entry(&self, conf: ExplainConfig) -> ExplainEntry {
        EntryBuilder::new("NoRows", conf)
            .with_values_if_verbose("table_refs", &self.table_refs)
            .build()
    }
}
