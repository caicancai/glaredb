//! Join ordering based on dynamic programming on hyper graphs.
//!
//! Follows the DPHyp algorithm for determining join order.
//!
//! See: <https://15721.courses.cs.cmu.edu/spring2020/papers/20-optimizer2/p539-moerkotte.pdf>
//!
//! The graph gets initalized with child plans, join conditions, and extra filters.
//! The conditions will be converted to edges between the base relations. Filters
//! are assumed to apply to a single node, and so will be added to a "built"
//! relations and are not considered to be edges.
//!
//! Join conditions are converted into "hyper edges" which contains a min NDV (Num
//! Distinct Values). This NDV is initialized to be the min cardinality of all base
//! relations connected by the hyper edge. The min NDV is used when computing the
//! estimated output cardinality of a join.
//!
//! Estimating the cardinality is done by dividing the product of all base relation
//! cardinalities taking part in the join by the a selectivity denominator. The
//! selectivity denominator is the product of all hyper edge NDVs taking part in the
//! join.
//!
//! The cost function is the estimated cardinality plus the cost of building up the
//! two child node.

use std::collections::{HashMap, HashSet};
use std::fmt;

use glaredb_error::{DbError, Result};

use super::ReorderableCondition;
use super::edge::{EdgeId, EdgeType, HyperEdges, NeighborEdge};
use super::statistics::propagate_estimated_cardinality;
use super::subgraph::Subgraph;
use crate::expr::Expression;
use crate::expr::conjunction_expr::{ConjunctionExpr, ConjunctionOperator};
use crate::logical::binder::bind_context::BindContext;
use crate::logical::binder::table_list::TableRef;
use crate::logical::logical_filter::LogicalFilter;
use crate::logical::logical_join::{
    JoinType,
    LogicalArbitraryJoin,
    LogicalComparisonJoin,
    LogicalCrossJoin,
};
use crate::logical::operator::{LocationRequirement, LogicalNode, LogicalOperator, Node};
use crate::optimizer::filter_pushdown::extracted_filter::ExtractedFilter;
use crate::statistics::assumptions::DEFAULT_SELECTIVITY;
use crate::statistics::value::StatisticsValue;
use crate::util::fmt::displayable::IntoDisplayableSlice;

/// Unique id for identifying base relations in the graph.
pub type RelId = usize;

/// Unique id for extra filters in the graph.
pub type FilterId = usize;

/// Tracks filters that have been used thus far in a particular join ordering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsedFilters {
    /// Complete set of filters used.
    pub filters: HashSet<FilterId>,
}

impl UsedFilters {
    /// Creates a set of used edges from two existing sets.
    fn unioned(left: &UsedFilters, right: &UsedFilters) -> Self {
        UsedFilters {
            filters: left
                .filters
                .iter()
                .chain(right.filters.iter())
                .copied()
                .collect(),
        }
    }

    fn mark_filters_used(&mut self, filters: impl IntoIterator<Item = FilterId>) {
        self.filters.extend(filters)
    }
}

#[derive(Debug)]
pub struct BaseRelation {
    pub rel_id: RelId,
    pub operator: LogicalOperator,
    pub output_refs: HashSet<TableRef>,
    pub cardinality: f64,
}

#[derive(Debug, Clone)]
pub struct JoinNode {
    /// The set of base relations making up this node.
    pub set: RelationSet,
    /// The cost for producing this node.
    pub cost: f64,
    /// Left child relations.
    pub left: RelationSet,
    /// Right child relations.
    pub right: RelationSet,
    /// Current subgraph for estimating cardinality.
    pub subgraph: Subgraph,
    /// Output table refs for this plan.
    ///
    /// Union of all child output refs.
    pub output_refs: HashSet<TableRef>,
    /// Edges containing the conditions that should be used when joining left
    /// and right.
    ///
    /// Empty when just a base relation.
    pub edges: HashSet<EdgeId>,
    /// Filters that will be applied to the left input.
    ///
    /// Empty when just a base relation.
    pub left_filters: HashSet<FilterId>,
    /// Filters that will be applied to the right input.
    ///
    /// Empty when just a base relation.
    pub right_filters: HashSet<FilterId>,
    /// Complete set of used edges up to and including this plan.
    ///
    /// Union of all edges used in children.
    ///
    /// This lets us track which filters/conditions we have used so far when
    /// considering this join order. We don't want to reuse filters/conditions
    /// within a join order.
    pub used: UsedFilters,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelationSet {
    pub relation_indices: Vec<usize>,
}

impl RelationSet {
    fn new(indices: impl IntoIterator<Item = usize>) -> Self {
        let mut indices: Vec<_> = indices.into_iter().collect();
        indices.sort_unstable();

        RelationSet {
            relation_indices: indices,
        }
    }

    fn empty() -> Self {
        RelationSet {
            relation_indices: Vec::new(),
        }
    }

    fn base(idx: usize) -> Self {
        RelationSet {
            relation_indices: vec![idx],
        }
    }

    fn is_base(&self) -> bool {
        self.relation_indices.len() == 1
    }

    fn union(left: &RelationSet, right: &RelationSet) -> Self {
        let mut indices: Vec<_> = left
            .relation_indices
            .iter()
            .chain(right.relation_indices.iter())
            .copied()
            .collect();
        indices.sort_unstable();
        indices.dedup();

        RelationSet {
            relation_indices: indices,
        }
    }

    fn get_all_neighbor_sets(mut neighbors: Vec<usize>) -> Vec<RelationSet> {
        fn add_supersets(current: &[HashSet<usize>], neighbors: &[usize]) -> Vec<HashSet<usize>> {
            let mut added = Vec::new();

            for neighbor_set in current {
                // Find the maximum value in the current neighbor set
                let max = neighbor_set.iter().max().unwrap();
                for &neighbor in neighbors {
                    if *max >= neighbor {
                        continue;
                    }
                    if !neighbor_set.contains(&neighbor) {
                        // Create a new set by adding the neighbor.
                        let mut new_set = neighbor_set.clone();
                        new_set.insert(neighbor);
                        added.push(new_set);
                    }
                }
            }

            added
        }

        let mut sets = Vec::new();
        neighbors.sort();
        let mut added = Vec::new();

        // Initialize with sets containing each neighbor individually
        for &neighbor in &neighbors {
            let mut set = HashSet::new();
            set.insert(neighbor);
            added.push(set.clone());
            sets.push(set);
        }

        // Generate all supersets
        while !added.is_empty() {
            added = add_supersets(&added, &neighbors);
            for d in &added {
                sets.push(d.clone());
            }
        }

        sets.into_iter().map(RelationSet::new).collect()
    }
}

impl fmt::Display for RelationSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.relation_indices.display_with_brackets())
    }
}

#[derive(Debug)]
pub struct Graph {
    /// Hyper edges in the graph.
    ///
    /// This contains the join conditions for joining nodes.
    hyper_edges: HyperEdges,
    /// Misc filters that only apply to a single node.
    ///
    /// What is a valid join condition vs what is just a simple filter should
    /// have been determined during filter pushdown.
    filters: HashMap<FilterId, ExtractedFilter>,
    /// Base input relations.
    base_relations: HashMap<RelId, BaseRelation>,
    /// Best join node plans we've found for the given set of relations.
    best_plans: HashMap<RelationSet, JoinNode>,
    /// Count of join pairs considered.
    pairs_considered: usize,
}

impl Graph {
    pub fn try_new(
        base_ops: impl IntoIterator<Item = LogicalOperator>,
        conditions: impl IntoIterator<Item = ReorderableCondition>,
        filters: impl IntoIterator<Item = ExtractedFilter>,
        bind_context: &BindContext,
    ) -> Result<Self> {
        let base_ops = base_ops
            .into_iter()
            .map(|mut op| {
                propagate_estimated_cardinality(&mut op)?;
                Ok(op)
            })
            .collect::<Result<Vec<_>>>()?;

        let base_relations: HashMap<RelId, BaseRelation> = base_ops
            .into_iter()
            .enumerate()
            .map(|(rel_id, op)| {
                let output_refs = op.get_output_table_refs(bind_context).into_iter().collect();
                let cardinality = op
                    .estimated_cardinality()
                    .value()
                    .copied()
                    .unwrap_or(20_000) as f64;

                (
                    rel_id,
                    BaseRelation {
                        rel_id,
                        operator: op,
                        output_refs,
                        cardinality,
                    },
                )
            })
            .collect();

        let hyper_edges = HyperEdges::new(conditions, &base_relations)?;

        let filters = filters.into_iter().enumerate().collect();

        let mut best_plans = HashMap::with_capacity(base_relations.len());

        for (&rel_id, base_rel) in &base_relations {
            // Initial subgraph, we're selecting everything from the base
            // relation.
            let subgraph = Subgraph {
                numerator: base_rel.cardinality,
                selectivity_denom: 1.0,
            };

            let rel_set = RelationSet::base(rel_id);

            let node = JoinNode {
                set: rel_set.clone(),
                cost: 0.0,
                left: RelationSet::empty(),
                right: RelationSet::empty(),
                subgraph,
                output_refs: base_rel.output_refs.clone(),
                edges: HashSet::new(),
                left_filters: HashSet::new(),
                right_filters: HashSet::new(),
                used: UsedFilters::default(),
            };

            best_plans.insert(rel_set, node);
        }

        Ok(Graph {
            hyper_edges,
            filters,
            best_plans,
            base_relations,
            pairs_considered: 0,
        })
    }

    pub fn try_build(mut self) -> Result<LogicalOperator> {
        self.solve()?;

        let longest_set = RelationSet::new(0..self.base_relations.len());

        // Check if we were able to actually create a best plan for the longest
        // set. If we weren't, then there's a disjunction in the relations.
        //
        // Generated edges for each relation such that cross joins are possible
        // at every level.
        if !self.best_plans.contains_key(&longest_set) {
            for i in 0..self.base_relations.len() {
                for j in 0..self.base_relations.len() {
                    if i == j {
                        continue;
                    }

                    let left = self.base_relations.get(&i).unwrap();
                    let right = self.base_relations.get(&j).unwrap();

                    self.hyper_edges.insert_cross_product(left, right);
                }
            }

            // TODO: Heuristic for if we should even try to solve. When running
            // this for 10 cross joins, we end up considering >50,000 pairs.

            // Try to solve again, now with the new edges. Failing to solve this
            // indicates a bigger problem, as the graph is now fully connected
            // and should be solveable.
            self.solve()?;
        }

        let longest = self
            .best_plans
            .remove(&longest_set)
            .ok_or_else(|| DbError::new("Missing longest best plan"))?;

        let plan = self.build_from_generated(longest)?;

        // All base relations and edges should have been used to build up the
        // plan.
        assert!(self.base_relations.is_empty());
        assert!(self.hyper_edges.all_non_empty_edges_removed());

        // But we may still have filters. Apply them to the final plan.
        let filter_ids: HashSet<_> = self.filters.keys().copied().collect();
        let plan = self.apply_filters(plan, &filter_ids)?;

        assert!(self.filters.is_empty());

        Ok(plan)
    }

    /// Solve the join order.
    ///
    /// Once solved, `best_plans` will have a generated join node containing the
    /// optimal join order.
    fn solve(&mut self) -> Result<()> {
        // Iterate over base relations to produce possible pairs.
        for base_idx in (0..self.base_relations.len()).rev() {
            let base_rel = RelationSet::base(base_idx);

            // Emit start.
            self.emit_connected_subgraphs(&base_rel)?;

            // Exclude all other base relations less than this relation's index.
            let exclude: HashSet<_> = (0..base_idx).collect();

            // Search for connected subgraphs starting from this relation.
            self.enumerate_connected_subgraphs_rec(&base_rel, &exclude)?;
        }

        Ok(())
    }

    /// Iterates a connected subgraph, emitting connected pairs.
    fn emit_connected_subgraphs(&mut self, set: &RelationSet) -> Result<()> {
        if set.relation_indices.len() == self.base_relations.len() {
            // Already at largest subgraph.
            return Ok(());
        }

        // Create an exclusion set for all relations "previous" to this set, as
        // well as all relations within this set.
        let mut exclude: HashSet<_> = (0..set.relation_indices[0]).collect();
        for idx in &set.relation_indices {
            exclude.insert(*idx);
        }

        let mut neighbors = self.hyper_edges.find_neighbors(set, &exclude);
        neighbors.sort_unstable_by(|a, b| a.cmp(b).reverse());

        // Add neighbors to exlusion set, as we're going to be working them in
        // the recursive call.
        exclude.extend(&neighbors);

        for neighbor in neighbors {
            // Find edges between this node and neighbor.
            let neighbor_set = RelationSet::base(neighbor);
            let edges = self.hyper_edges.find_edges(set, &neighbor_set);

            if !edges.is_empty() {
                // We have a connection.
                self.emit_pair(set, &neighbor_set, edges)?;
            }

            // Kick off recursively visiting neighbors.
            self.enumerate_connected_complement_rec(set, &neighbor_set, &exclude)?;

            exclude.remove(&neighbor);
        }

        Ok(())
    }

    fn enumerate_connected_subgraphs_rec(
        &mut self,
        set: &RelationSet,
        exclude: &HashSet<usize>,
    ) -> Result<()> {
        let neighbors = self.hyper_edges.find_neighbors(set, exclude);
        if neighbors.is_empty() {
            return Ok(());
        }

        let neighbor_sets = RelationSet::get_all_neighbor_sets(neighbors.clone());
        let mut combined_sets = Vec::with_capacity(neighbor_sets.len());

        for neigbor_set in neighbor_sets {
            let combined = RelationSet::union(set, &neigbor_set);
            if self.best_plans.contains_key(&combined) {
                self.emit_connected_subgraphs(&combined)?;
            }

            combined_sets.push(combined);
        }

        let mut exclude = exclude.clone();
        exclude.extend(neighbors);

        for combined in combined_sets {
            self.enumerate_connected_subgraphs_rec(&combined, &exclude)?;
        }

        Ok(())
    }

    fn enumerate_connected_complement_rec(
        &mut self,
        left: &RelationSet,
        right: &RelationSet,
        exclude: &HashSet<usize>,
    ) -> Result<()> {
        let neighbors = self.hyper_edges.find_neighbors(right, exclude);
        if neighbors.is_empty() {
            return Ok(());
        }

        let neighbor_sets = RelationSet::get_all_neighbor_sets(neighbors.clone());
        let mut combined_sets = Vec::with_capacity(neighbor_sets.len());

        for neigbor_set in neighbor_sets {
            let combined = RelationSet::union(right, &neigbor_set);

            assert!(combined.relation_indices.len() > right.relation_indices.len());

            if self.best_plans.contains_key(&combined) {
                let edges = self.hyper_edges.find_edges(left, &combined);

                if !edges.is_empty() {
                    // println!("COMP");
                    self.emit_pair(left, &combined, edges)?;
                }
            }

            combined_sets.push(combined);
        }

        // Extend exclusion to include neighbors we just visited.
        let mut exclude = exclude.clone();
        exclude.extend(neighbors);

        // Recurse into the combined neighborder sets.
        for combined in combined_sets {
            self.enumerate_connected_complement_rec(left, &combined, &exclude)?;
        }

        Ok(())
    }

    /// Emits a join pair between left and right.
    ///
    /// If the cost for the join is lower than the current best for the union of
    /// left and right, this join pair will replace the existing pair.
    fn emit_pair(
        &mut self,
        left: &RelationSet,
        right: &RelationSet,
        edges: Vec<NeighborEdge>,
    ) -> Result<()> {
        self.pairs_considered += 1;

        let left = self
            .best_plans
            .get_key_value(left)
            .ok_or_else(|| DbError::new("missing best plan for left"))?;
        let right = self
            .best_plans
            .get_key_value(right)
            .ok_or_else(|| DbError::new("missing best plan for right"))?;

        let new_set = RelationSet::union(left.0, right.0);

        let left_filters = self.find_filters(left.1);
        let right_filters = self.find_filters(right.1);

        // Clone the left subgraph, and modify it to account for the
        // joins with the right based on edges.
        //
        // For semi joins, we only want to update from one side of subgraph.
        let mut subgraph = left.1.subgraph;
        let any_semi = edges
            .iter()
            .any(|edge| matches!(edge.edge_op, EdgeType::Semi));
        // TODO: This needs to account for edge direction (if left side of the
        // edge actually is from the left plan and not flipped). Same with
        // denom.
        if !any_semi {
            subgraph.update_numerator(&right.1.subgraph);
        }

        // Update just the numerator to account for the change in cardinality.
        //
        // We cannot assume that the filter is filtering on the same attributes
        // we're joining on, so avoid updating the denominator.
        for _ in 0..left_filters.len() + right_filters.len() {
            subgraph.numerator *= DEFAULT_SELECTIVITY;
        }

        // Update denominator based on edges used for this join.
        //
        // Only the "max" edge is considered to avoid assuming edges are
        // independent of one another. Since we're generating additional filters
        // in a previous optimization step, it's safe to assume there's going to
        // be some number of conditions that are related.
        let edge = edges
            .iter()
            .max_by(|a, b| f64::total_cmp(&a.min_ndv, &b.min_ndv));

        // If no edge, cardinality output will just be cross product.
        if let Some(edge) = edge {
            subgraph.update_denom(&right.1.subgraph, edge);
        }

        // Get the estimated cardinality at this point in the
        // subgraph construction.
        let cardinality = subgraph.estimated_cardinality();

        // Simple cost function.
        //
        // This is additive to ensure we fully include the cost of
        // all other joins making up this plan.
        let cost = cardinality + left.1.cost + right.1.cost;

        // Check to see if this cost is lower than existing cost. Returns early
        // if not.
        if let Some(existing) = self.best_plans.get(&new_set) {
            if existing.cost < cost {
                return Ok(());
            }
        }

        // New node is better. Create it and insert into plans.

        let left_filters: HashSet<_> = left_filters.iter().map(|&(&id, _)| id).collect();
        let right_filters: HashSet<_> = right_filters.iter().map(|&(&id, _)| id).collect();

        let edges: HashSet<_> = edges.iter().map(|edge| edge.edge_id).collect();

        let mut used = UsedFilters::unioned(&left.1.used, &right.1.used);
        used.mark_filters_used(left_filters.iter().copied());
        used.mark_filters_used(right_filters.iter().copied());

        let output_refs: HashSet<_> = left
            .1
            .output_refs
            .iter()
            .chain(&right.1.output_refs)
            .copied()
            .collect();

        self.best_plans.insert(
            new_set.clone(),
            JoinNode {
                set: new_set,
                cost,
                left: left.0.clone(),
                right: right.0.clone(),
                subgraph,
                output_refs,
                edges,
                left_filters,
                right_filters,
                used,
            },
        );

        Ok(())
    }

    /// Find filters that apply fully to the given plan.
    fn find_filters(&self, node: &JoinNode) -> Vec<(&FilterId, &ExtractedFilter)> {
        self.filters
            .iter()
            .filter(|(filter_id, filter)| {
                // Constant filter, this should be applied to the top of the
                // plan. An optimizer rule should be written to prune out
                // constant filters.
                //
                // We're currently assuming that a filter needs to be used
                // extactly once in the tree. And this check enforces that.
                if filter.table_refs.is_empty() {
                    return false;
                }

                // Only consider filters not yet used.
                if node.used.filters.contains(filter_id) {
                    return false;
                }

                // Only consider filters that apply to just the plan's table refs.
                if !filter.table_refs.is_subset(&node.output_refs) {
                    return false;
                }

                // Usable filter.
                true
            })
            .collect()
    }

    /// Apply filters to a plan we're building up.
    ///
    /// Errors if any of the filters were previously used.
    fn apply_filters(
        &mut self,
        input: LogicalOperator,
        filters: &HashSet<FilterId>,
    ) -> Result<LogicalOperator> {
        if filters.is_empty() {
            // Nothing to do.
            return Ok(input);
        }

        let mut input_filters = Vec::with_capacity(filters.len());

        for filter_id in filters {
            let filter = self
                .filters
                .remove(filter_id)
                .ok_or_else(|| DbError::new(format!("Filter previously used: {filter_id}")))?;

            input_filters.push(filter.filter);
        }

        // Try to squash into underlying operator if able. Otherwise just wrap
        // in a filter.
        match input {
            LogicalOperator::Filter(filter) => {
                let filter_expr = Expression::Conjunction(ConjunctionExpr {
                    op: ConjunctionOperator::And,
                    expressions: input_filters
                        .into_iter()
                        .chain([filter.node.filter])
                        .collect(),
                });

                Ok(LogicalOperator::Filter(Node {
                    node: LogicalFilter {
                        filter: filter_expr,
                    },
                    location: filter.location,
                    children: filter.children,
                    estimated_cardinality: StatisticsValue::Unknown,
                }))
            }
            LogicalOperator::ArbitraryJoin(join) if join.node.join_type == JoinType::Inner => {
                let condition = Expression::Conjunction(ConjunctionExpr {
                    op: ConjunctionOperator::And,
                    expressions: input_filters
                        .into_iter()
                        .chain([join.node.condition])
                        .collect(),
                });

                Ok(LogicalOperator::ArbitraryJoin(Node {
                    node: LogicalArbitraryJoin {
                        join_type: JoinType::Inner,
                        condition,
                    },
                    location: join.location,
                    children: join.children,
                    estimated_cardinality: StatisticsValue::Unknown,
                }))
            }
            LogicalOperator::CrossJoin(join) => {
                let condition = Expression::Conjunction(ConjunctionExpr {
                    op: ConjunctionOperator::And,
                    expressions: input_filters,
                });

                Ok(LogicalOperator::ArbitraryJoin(Node {
                    node: LogicalArbitraryJoin {
                        join_type: JoinType::Inner,
                        condition,
                    },
                    location: join.location,
                    children: join.children,
                    estimated_cardinality: StatisticsValue::Unknown,
                }))
            }
            other => {
                let filter = Expression::Conjunction(ConjunctionExpr {
                    op: ConjunctionOperator::And,
                    expressions: input_filters,
                });

                Ok(LogicalOperator::Filter(Node {
                    node: LogicalFilter { filter },
                    location: LocationRequirement::Any,
                    children: vec![other],
                    estimated_cardinality: StatisticsValue::Unknown,
                }))
            }
        }
    }

    /// Recursively builds up the logical operator from a generated join node.
    fn build_from_generated(&mut self, mut node: JoinNode) -> Result<LogicalOperator> {
        // If we're building a base relation, we can just return the relation
        // as-is. No other field should have been populated.
        if node.set.is_base() {
            assert!(node.edges.is_empty());

            // There should always be one generated plan referencing the base
            // relation.
            let rel = self
                .base_relations
                .remove(&node.set.relation_indices[0])
                .ok_or_else(|| DbError::new("Missing base relation"))?;

            return Ok(rel.operator);
        }

        // Otherwise build up the plan.

        let mut left_gen = self
            .best_plans
            .remove(&node.left)
            .ok_or_else(|| DbError::new("Missing left input"))?;
        let mut right_gen = self
            .best_plans
            .remove(&node.right)
            .ok_or_else(|| DbError::new("Missing right input"))?;

        // If any of the conditions are part of a semi join, ensure the correct
        // order of the plans and prevent swapping sides.
        let mut any_semi = false;
        for &edge_id in &node.edges {
            let edge = self
                .hyper_edges
                .get_edge(edge_id)
                .ok_or_else(|| DbError::new("Missing edge"))?;

            let mut node_flipped = false;

            if let Some(cond @ ReorderableCondition::Semi { .. }) = &edge.filter {
                let [left_refs, _right_refs] = cond.get_left_right_table_refs();

                if !left_refs.is_subset(&left_gen.output_refs) {
                    // Need to swap to get the plans on the right side.
                    std::mem::swap(&mut left_gen, &mut right_gen);
                    node_flipped = true;
                }

                any_semi = true;
            }

            // We need to swap the filters as well. Do this only once,
            // as they're a property of the join node. If we happen to
            // have more than one semi join (possible?), then we don't
            // want to undo the swap.
            if node_flipped {
                std::mem::swap(&mut node.left, &mut node.right);
                std::mem::swap(&mut node.left_filters, &mut node.right_filters);

                // TODO: Double check that it's fine to flip the conditions
                // below and not here.
            }
        }

        let mut conditions = Vec::with_capacity(node.edges.len());
        for &edge_id in &node.edges {
            let edge = self
                .hyper_edges
                .remove_edge(edge_id)
                .ok_or_else(|| DbError::new("Edge already used"))?;

            let condition = match edge.filter {
                Some(filter) => filter,
                None => {
                    // No condition on this edge, assume cross join.
                    continue;
                }
            };

            match condition {
                ReorderableCondition::Inner { mut condition } => {
                    let condition_swap_sides = edge.left_refs.is_subset(&right_gen.output_refs);
                    if condition_swap_sides {
                        condition.flip_sides();
                    }

                    conditions.push(condition);
                }
                ReorderableCondition::Semi {
                    conditions: mut semi_conditions,
                } => {
                    // Even though can't (yet) freely flip semi joins, the edges
                    // themselves don't convey node order. So figure out the
                    // correct condition using the table refs.
                    let condition_swap_sides = edge.left_refs.is_subset(&right_gen.output_refs);
                    if condition_swap_sides {
                        for condition in &mut semi_conditions {
                            condition.flip_sides();
                        }
                    }

                    conditions.append(&mut semi_conditions);
                }
            }
        }

        // Determine if we should swap sides. We always want left (build) side
        // to have the lower cardinality (not necessarily cost).
        //
        // Don't swap sides yet, still need to apply filters.
        let plan_swap_sides = (!any_semi)
            && right_gen.subgraph.estimated_cardinality()
                < left_gen.subgraph.estimated_cardinality();

        let left = self.build_from_generated(left_gen)?;
        let right = self.build_from_generated(right_gen)?;

        let left = self.apply_filters(left, &node.left_filters)?;
        let right = self.apply_filters(right, &node.right_filters)?;

        let [left, right] = if plan_swap_sides {
            [right, left]
        } else {
            [left, right]
        };

        // If we swapped sides, we'll need to flip the join conditions to match.
        if plan_swap_sides {
            for cond in &mut conditions {
                cond.flip_sides();
            }
        }

        if conditions.is_empty() {
            // No conditions, simple cross join.
            Ok(LogicalOperator::CrossJoin(Node {
                node: LogicalCrossJoin,
                location: LocationRequirement::Any,
                children: vec![left, right],
                estimated_cardinality: StatisticsValue::Unknown,
            }))
        } else {
            let join_type = if any_semi {
                JoinType::LeftSemi
            } else {
                JoinType::Inner
            };

            // We have conditions, create comparison join.
            Ok(LogicalOperator::ComparisonJoin(Node {
                node: LogicalComparisonJoin {
                    join_type,
                    conditions,
                },
                location: LocationRequirement::Any,
                children: vec![left, right],
                estimated_cardinality: StatisticsValue::Estimated(
                    node.subgraph.estimated_cardinality() as usize,
                ),
            }))
        }
    }
}
