use glaredb_error::Result;

use super::OperatorPlanState;
use crate::execution::operators::sort::global_sort::PhysicalGlobalSort;
use crate::execution::operators::{PlannedOperator, PlannedOperatorWithChildren};
use crate::logical::logical_order::LogicalOrder;
use crate::logical::operator::{LogicalNode, Node};

impl OperatorPlanState<'_> {
    pub fn plan_sort(
        &mut self,
        mut order: Node<LogicalOrder>,
    ) -> Result<PlannedOperatorWithChildren> {
        let _location = order.location;

        let input = order.take_one_child_exact()?;
        let input_refs = input.get_output_table_refs(self.bind_context);
        let child = self.plan(input)?;

        let sort_exprs = self
            .expr_planner
            .plan_sorts(&input_refs, &order.node.exprs)?;

        let sort = PhysicalGlobalSort::new(
            sort_exprs,
            child.operator.call_output_types(),
            order.node.limit_hint,
        );

        Ok(PlannedOperatorWithChildren {
            operator: PlannedOperator::new_execute(self.id_gen.next_id(), sort),
            children: vec![child],
        })
    }
}
