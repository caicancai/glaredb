use std::collections::HashMap;

use glaredb_error::{DbError, Result};
use glaredb_parser::ast::{self};

use super::bind_group_by::BoundGroupBy;
use super::bind_select_list::SelectListBinder;
use crate::expr::Expression;
use crate::expr::column_expr::{ColumnExpr, ColumnReference};
use crate::logical::binder::bind_context::BindContext;
use crate::logical::binder::ident::BinderIdent;
use crate::logical::binder::table_list::TableRef;
use crate::logical::logical_aggregate::GroupingFunction;
use crate::logical::resolver::ResolvedMeta;

// TODO: On
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundDistinctModifier {
    /// Distinct on all columns,
    Distinct,
    /// No distinct, return all rows.
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputProjectionTable {
    /// Table containing just column references.
    pub table: TableRef,
    /// Column expressions containing references to the original expanded select
    /// expressions.
    pub expressions: Vec<Expression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundSelectList {
    /// Optional ouptut table to use at the end of select planning.
    ///
    /// Is Some when additional columns are added to the select list for ORDER
    /// BY and GROUP BY. The pruned table serves to remove those from the final
    /// output.
    ///
    /// In the case of window functions, an output tabe will be required as
    /// windows need to be executed after aggregates/having. Window expressions
    /// in the original projection list will have been replaced with a column
    /// reference referencing the window table.
    pub output: Option<OutputProjectionTable>,
    /// Table containing columns for projections
    pub projections_table: TableRef,
    /// Projection expressions. May contain additional expressions for use with
    /// ORDER BY, GROUP BY or window functions.
    pub projections: Vec<Expression>,
    /// Table containing columns for aggregates.
    pub aggregates_table: TableRef,
    /// All extracted aggregates.
    pub aggregates: Vec<Expression>,
    /// Table containing columns for windows.
    pub windows_table: TableRef,
    /// All extracted windows.
    pub windows: Vec<Expression>,
    /// Output table for group indices for GROUPING calls.
    pub grouping_functions_table: TableRef,
    /// List of grouping functions.
    pub grouping_functions: Vec<GroupingFunction>,
    /// Distinct modifer.
    ///
    /// If ALL, then a normal projection is used. If DISTINCT, then a distinct
    /// project will be used.
    pub distinct_modifier: BoundDistinctModifier,
}

#[derive(Debug)]
pub struct SelectList {
    /// The table scope that expressions referencing columns in the select list
    /// should bind to.
    ///
    /// Remains empty during binding.
    pub projections_table: TableRef,
    /// Mapping from explicit user-provided alias to column index in the output.
    pub alias_map: HashMap<BinderIdent, usize>,
    /// Expanded projections that will be shown in the output.
    pub projections: Vec<Expression>,
    /// Projections that are appended to the right of the output projects.
    ///
    /// This is for appending expressions used for ORDER BY and GROUP BY.
    ///
    /// Appending to this list may introduce new aggregates, but not windows.
    pub appended: Vec<Expression>,
    /// Table containing columns for aggregates.
    pub aggregates_table: TableRef,
    /// All extracted aggregates.
    pub aggregates: Vec<Expression>,
    /// Table containing columns for windows.
    pub windows_table: TableRef,
    /// All extracted windows.
    pub windows: Vec<Expression>,
    /// Table for the group output for use with GROUPING calls.
    pub grouping_functions_table: TableRef,
    /// All extracted GROUPING function calls.
    pub grouping_set_references: Vec<Expression>,
    /// Distinct modifer from the select list.
    pub distinct_modifier: BoundDistinctModifier,
}

impl SelectList {
    /// Finalizes the select list, producing a bound variant.
    ///
    /// This will extract aggregates from the list, placing them in their own
    /// table, and add pruning projections if needed.
    pub fn finalize(
        mut self,
        bind_context: &mut BindContext,
        mut group_by: Option<&mut BoundGroupBy>,
    ) -> Result<BoundSelectList> {
        let grouping_functions = self.expressions_to_grouping_functions(&group_by)?;

        // Have projections point to the GROUP BY instead of the other way
        // around.
        if let Some(group_by) = group_by.as_mut() {
            self.update_group_by_dependencies(group_by)?;
        }

        self.verify_column_references(
            bind_context,
            self.aggregates_table,
            self.grouping_functions_table,
            &self.aggregates,
            group_by,
        )?;

        // Update names in the current projection scope to only be the
        // user-provided alias. Finalizing the select list means we're finalize
        // the subquery, and we should only expose the user alias, not the
        // original name.
        let projections_table = bind_context.get_table_mut(self.projections_table)?;
        for (alias, col_idx) in self.alias_map {
            let col_name = projections_table.column_names.get_mut(col_idx).ok_or_else(|| DbError::new(format!("Attempted to set the column alias '{alias}' for out-of-bounds column {col_idx}")))?;
            *col_name = alias;
        }

        // If we had appended column, ensure we have a pruned table that only
        // contains the original projections.
        let pruned_table = if !self.appended.is_empty() {
            let len = self.projections.len();

            // Move appended expressions into the projections list.
            self.projections.append(&mut self.appended);

            let projections_table = bind_context.get_table(self.projections_table)?;
            let types: Vec<_> = projections_table
                .column_types
                .iter()
                .take(len)
                .cloned()
                .collect();
            let names: Vec<_> = projections_table
                .column_names
                .iter()
                .take(len)
                .cloned()
                .collect();
            let output_table_ref = bind_context.new_ephemeral_table_with_columns(types, names)?;

            // Project out only expressions in the original select list.
            let projections_table = bind_context.get_table(self.projections_table)?;

            let expressions = (0..len)
                .map(|idx| {
                    Expression::Column(ColumnExpr {
                        reference: ColumnReference {
                            table_scope: self.projections_table,
                            column: idx,
                        },
                        datatype: projections_table.column_types[idx].clone(),
                    })
                })
                .collect();

            Some(OutputProjectionTable {
                table: output_table_ref,
                expressions,
            })
        } else {
            None
        };

        Ok(BoundSelectList {
            output: pruned_table,
            projections_table: self.projections_table,
            projections: self.projections,
            aggregates_table: self.aggregates_table,
            aggregates: self.aggregates,
            windows_table: self.windows_table,
            windows: self.windows,

            grouping_functions_table: self.grouping_functions_table,
            grouping_functions,

            distinct_modifier: self.distinct_modifier,
        })
    }

    /// Verify that all column expressions in the project list point to either
    /// an aggregate or group by expression.
    fn verify_column_references(
        &self,
        bind_context: &BindContext,
        agg_table: TableRef,
        groupings_table: TableRef,
        aggs: &[Expression],
        group_by: Option<&mut BoundGroupBy>,
    ) -> Result<()> {
        if aggs.is_empty() && group_by.is_none() {
            return Ok(());
        }

        fn inner(bind_context: &BindContext, expr: &Expression, refs: &[TableRef]) -> Result<()> {
            match expr {
                Expression::Column(col) => {
                    if !refs
                        .iter()
                        .any(|table_ref| &col.reference.table_scope == table_ref)
                    {
                        let (col_name, _) = bind_context.get_column(col.reference)?;
                        return Err(DbError::new(format!(
                            "Column '{col_name}' must appear in the GROUP BY clause or be used in an aggregate function"
                        )));
                    }
                }
                other => other.for_each_child(|child| inner(bind_context, child, refs))?,
            }
            Ok(())
        }

        match group_by {
            Some(group_by) => {
                for expr in self.projections.iter().chain(&self.appended) {
                    // Expression needs to reference:
                    // - An aggregate
                    // - An expression in the group by
                    // - A GROUPING call
                    inner(
                        bind_context,
                        expr,
                        &[agg_table, group_by.group_exprs_table, groupings_table],
                    )?
                }
            }
            None => {
                for expr in self.projections.iter().chain(&self.appended) {
                    inner(bind_context, expr, &[agg_table])?
                }
            }
        }

        Ok(())
    }

    /// Appends an expression to the select list.
    ///
    /// This will automatically extract out any aggregates from the expression.
    pub fn append_projection(
        &mut self,
        bind_context: &mut BindContext,
        mut expr: Expression,
    ) -> Result<ColumnExpr> {
        let datatype = expr.datatype()?;

        SelectListBinder::extract_aggregates(
            self.aggregates_table,
            self.grouping_functions_table,
            bind_context,
            &mut expr,
            &mut self.aggregates,
            &mut self.grouping_set_references,
        )?;

        self.appended.push(expr);
        let idx = bind_context.push_column_for_table(
            self.projections_table,
            "__appended_proj",
            datatype.clone(),
        )?;

        Ok(ColumnExpr {
            reference: ColumnReference {
                table_scope: self.projections_table,
                column: idx,
            },
            datatype,
        })
    }

    /// Try to get a column by either a user-provided alias or the column name
    ///
    /// User aliases get preference.
    ///
    /// Returns Ok(None) if a column with the given ident alias or name doesn't
    /// exist.
    pub fn column_by_user_alias_or_name(
        &self,
        bind_context: &BindContext,
        ident: &BinderIdent,
    ) -> Result<Option<ColumnExpr>> {
        // Check user provided alias first.
        if let Some(idx) = self.alias_map.get(ident) {
            let reference = ColumnReference {
                table_scope: self.projections_table,
                column: *idx,
            };
            let datatype = bind_context.get_column_type(reference)?;

            return Ok(Some(ColumnExpr {
                reference,
                datatype,
            }));
        }

        // Now check the bind scope of the projections to see if we're
        // referencing something there.
        let mut column_expr = None;
        let table = bind_context.get_table(self.projections_table)?;

        // Iter over all columns in the select list that were part of the
        // original selection.
        for (col_idx, (col_name, col_type)) in table
            .iter_names_and_types()
            .enumerate()
            .take(self.projections.len())
        {
            if ident == col_name {
                // We got our column!
                if column_expr.is_some() {
                    // But we got more than one...
                    return Err(DbError::new(format!(
                        "Ambiguous column name in select '{col_name}'"
                    )));
                }
                column_expr = Some(ColumnExpr {
                    reference: (table.reference, col_idx).into(),
                    datatype: col_type.clone(),
                })
            }
        }

        Ok(column_expr)
    }

    /// Get a column reference by ordinal.
    pub fn column_by_ordinal(
        &self,
        bind_context: &BindContext,
        lit: &ast::Literal<ResolvedMeta>,
    ) -> Result<Option<ColumnExpr>> {
        if let ast::Literal::Number(s) = lit {
            let n = s
                .parse::<i64>()
                .map_err(|_| DbError::new(format!("Failed to parse '{s}' into a number")))?;
            if n < 1 || n as usize > self.projections.len() {
                return Err(DbError::new(format!(
                    "Column out of range, expected 1 to {}",
                    self.projections.len()
                )))?;
            }

            // TODO: We might to check that the ordinal is referencing the
            // original columns, and not any of the appended ones.
            let reference = ColumnReference {
                table_scope: self.projections_table,
                column: n as usize - 1, // SQL is 1 indexed
            };
            let datatype = bind_context.get_column_type(reference)?;

            return Ok(Some(ColumnExpr {
                reference,
                datatype,
            }));
        }

        Ok(None)
    }

    /// Find the indices to use for the GROUPING expressions.
    ///
    /// The output vec length equals the number of GROUPING calls. The vec may
    /// contain duplicates.
    fn expressions_to_grouping_functions(
        &self,
        group_by: &Option<&mut BoundGroupBy>,
    ) -> Result<Vec<GroupingFunction>> {
        let group_by = match group_by {
            Some(group_by) => group_by,
            None => {
                if self.grouping_set_references.is_empty() {
                    return Ok(Vec::new());
                } else {
                    return Err(DbError::new(
                        "GROUPING cannot be used without any GROUP BY expressions",
                    ));
                }
            }
        };

        let mut grouping_functions = Vec::new();

        for grouping_expr in &self.grouping_set_references {
            let mut expr_indices = Vec::new();

            let grouping_expr = match grouping_expr {
                Expression::GroupingSet(expr) => expr,
                other => {
                    return Err(DbError::new(format!(
                        "Expected grouping set expression, got {other}"
                    )));
                }
            };

            // Find the expressions the inputs match with.
            for input in &grouping_expr.inputs {
                let idx = match group_by.expressions.iter().position(|expr| expr == input) {
                    Some(idx) => idx,
                    None => {
                        return Err(DbError::new(format!(
                            "'{input}' was not found in the GROUP BY and cannot be used as an argument to GROUPING"
                        )));
                    }
                };

                expr_indices.push(idx);
            }

            grouping_functions.push(GroupingFunction {
                group_exprs: expr_indices,
            });
        }

        Ok(grouping_functions)
    }

    /// Updates expressions in the select list and bound group to ensure the
    /// select list depends on columns in the group by, and not the other way
    /// around.
    ///
    /// During GROUP BY binding, we use a column reference pointing to the
    /// select list. We avoid cloning the expression directly into the GROUP BY
    /// since that'll cause some ambiguity around if the expression is a sub
    /// expression or not.
    fn update_group_by_dependencies(&mut self, group_by: &mut BoundGroupBy) -> Result<()> {
        // Update group expressions to be the base for any aliased expressions.
        for (idx, expr) in group_by.expressions.iter_mut().enumerate() {
            if let Expression::Column(col) = expr {
                if col.reference.table_scope == self.projections_table {
                    let proj_expr = self
                        .projections
                        .get_mut(col.reference.column)
                        .ok_or_else(|| DbError::new(format!("Missing projection column: {col}")))?;

                    let datatype = proj_expr.datatype()?;

                    // Point projection to GROUP BY expression, replace GROUP BY
                    // expression with original expression.
                    let orig = std::mem::replace(
                        proj_expr,
                        Expression::Column(ColumnExpr {
                            reference: ColumnReference {
                                table_scope: group_by.group_exprs_table,
                                column: idx,
                            },
                            datatype,
                        }),
                    );

                    *expr = orig;
                }
            }
        }

        fn update_projection_expr(
            group_by_expr: &Expression,
            group_by_col: &ColumnExpr,
            expr: &mut Expression,
        ) -> Result<()> {
            if expr == group_by_expr {
                *expr = Expression::Column(group_by_col.clone());
                return Ok(());
            }

            expr.for_each_child_mut(|child| {
                update_projection_expr(group_by_expr, group_by_col, child)
            })
        }

        // Now update all columns in the select list to references to the GROUP
        // BY expressions.
        for (idx, group_by_expr) in group_by.expressions.iter().enumerate() {
            let group_by_col = ColumnExpr {
                reference: ColumnReference {
                    table_scope: group_by.group_exprs_table,
                    column: idx,
                },
                datatype: group_by_expr.datatype()?,
            };

            for expr in self.projections.iter_mut().chain(self.appended.iter_mut()) {
                update_projection_expr(group_by_expr, &group_by_col, expr)?;
            }
        }

        Ok(())
    }
}
