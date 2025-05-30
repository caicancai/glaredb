use glaredb_error::Result;

use super::binder::bind_context::{BindContext, MaterializationRef};
use super::binder::table_list::TableRef;
use super::operator::{LogicalNode, Node};
use crate::explain::explainable::{EntryBuilder, ExplainConfig, ExplainEntry, Explainable};
use crate::expr::Expression;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalMaterializationScan {
    /// Reference to the materialization in the bind context.
    pub mat: MaterializationRef,
}

impl Explainable for LogicalMaterializationScan {
    fn explain_entry(&self, conf: ExplainConfig) -> ExplainEntry {
        EntryBuilder::new("MaterializationScan", conf)
            .with_value("materialization_ref", self.mat)
            .build()
    }
}

impl LogicalNode for Node<LogicalMaterializationScan> {
    fn name(&self) -> &'static str {
        "MaterializationScan"
    }

    fn get_output_table_refs(&self, bind_context: &BindContext) -> Vec<TableRef> {
        bind_context
            .get_materialization(self.node.mat)
            .map(|m| m.table_refs.clone())
            .unwrap_or_default() // TODO: Error?
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

/// An alternative materialized scan operator that projects out of the
/// materialization and removes duplicates.
///
/// This should only be found in the child of a magic join.
///
/// These are created during subquery decorrelation such that the branch in the
/// plan representing work for a subquery is working with deduplicated inputs.
///
/// Essentially this encodes a disctint, project, and materialized scan into a
/// single operator, e.g.:
/// ```text
/// DISTINCT column1, column2
///   PROJECT <expr> as column1, <expr> as column2
///      MATERIALIZED_SCAN ...
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalMagicMaterializationScan {
    /// Reference to the materialization in the bind context.
    pub mat: MaterializationRef,
    /// Projections out of the materialization scan that will have duplicates
    /// removed.
    pub projections: Vec<Expression>,
    /// The table ref for this scan.
    ///
    /// This operator exposes a new reference since all parent operators must
    /// reference the deduplicated projections, and nothing inside the
    /// materialization.
    pub table_ref: TableRef,
}

impl Explainable for LogicalMagicMaterializationScan {
    fn explain_entry(&self, conf: ExplainConfig) -> ExplainEntry {
        EntryBuilder::new("MagicMaterializationScan", conf)
            .with_value("materialization_ref", self.mat)
            .with_values("projections", &self.projections)
            .with_value_if_verbose("table_ref", self.table_ref)
            .build()
    }
}

impl LogicalNode for Node<LogicalMagicMaterializationScan> {
    fn name(&self) -> &'static str {
        "MagicMaterializationScan"
    }

    fn get_output_table_refs(&self, _bind_context: &BindContext) -> Vec<TableRef> {
        vec![self.node.table_ref]
    }

    fn for_each_expr<'a, F>(&'a self, mut func: F) -> Result<()>
    where
        F: FnMut(&'a Expression) -> Result<()>,
    {
        for expr in &self.node.projections {
            func(expr)?;
        }
        Ok(())
    }

    fn for_each_expr_mut<'a, F>(&'a mut self, mut func: F) -> Result<()>
    where
        F: FnMut(&'a mut Expression) -> Result<()>,
    {
        for expr in &mut self.node.projections {
            func(expr)?;
        }
        Ok(())
    }
}
