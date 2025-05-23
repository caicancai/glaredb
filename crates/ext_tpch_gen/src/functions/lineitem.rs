use glaredb_core::arrays::array::physical_type::{
    AddressableMut,
    MutableScalarStorage,
    PhysicalI32,
    PhysicalI64,
    PhysicalUtf8,
};
use glaredb_core::arrays::batch::Batch;
use glaredb_core::arrays::datatype::{DataType, DataTypeId, DecimalTypeMeta};
use glaredb_core::functions::Signature;
use glaredb_core::functions::documentation::{Category, Documentation};
use glaredb_core::functions::function_set::TableFunctionSet;
use glaredb_core::functions::table::RawTableFunction;
use glaredb_core::storage::projections::{ProjectedColumn, Projections};
use glaredb_error::{OptionExt, Result};
use tpchgen::generators::{LineItem, LineItemGenerator, LineItemGeneratorIterator};

use super::convert;
use super::table_gen::{TableGen, TpchColumn, TpchTable};

pub const FUNCTION_SET_LINEITEM: TableFunctionSet = TableFunctionSet {
    name: "lineitem",
    aliases: &[],
    doc: &[&Documentation {
        category: Category::Table,
        description: "Generates TPC-H lineitem data with the specified scale factor.",
        arguments: &["scale_factor"],
        example: None,
    }],
    functions: &[RawTableFunction::new_scan(
        &Signature::new(&[DataTypeId::Float64], DataTypeId::Table),
        &TableGen::new(LineItemTable),
    )],
};

#[derive(Debug, Clone, Copy)]
pub struct LineItemTable;

impl TpchTable for LineItemTable {
    const COLUMNS: &[TpchColumn] = &[
        TpchColumn::new("l_orderkey", DataType::int64()),
        TpchColumn::new("l_partkey", DataType::int64()),
        TpchColumn::new("l_suppkey", DataType::int64()),
        TpchColumn::new("l_linenumber", DataType::int32()),
        TpchColumn::new("l_quantity", DataType::int64()),
        TpchColumn::new(
            "l_extendedprice",
            DataType::decimal64(DecimalTypeMeta::new(15, 2)),
        ),
        TpchColumn::new(
            "l_discount",
            DataType::decimal64(DecimalTypeMeta::new(15, 2)),
        ),
        TpchColumn::new("l_tax", DataType::decimal64(DecimalTypeMeta::new(15, 2))),
        TpchColumn::new("l_returnflag", DataType::utf8()),
        TpchColumn::new("l_linestatus", DataType::utf8()),
        TpchColumn::new("l_shipdate", DataType::date32()),
        TpchColumn::new("l_commitdate", DataType::date32()),
        TpchColumn::new("l_receiptdate", DataType::date32()),
        TpchColumn::new("l_shipinstruct", DataType::utf8()),
        TpchColumn::new("l_shipmode", DataType::utf8()),
        TpchColumn::new("l_comment", DataType::utf8()),
    ];

    type RowIter = LineItemGeneratorIterator<'static>;
    type Row = LineItem<'static>;

    fn create_row_iter(sf: Option<f64>) -> Result<Self::RowIter> {
        let sf = sf.required("sf")?;
        Ok(LineItemGenerator::new(sf, 1, 1).iter())
    }

    fn scan(rows: &[Self::Row], projections: &Projections, output: &mut Batch) -> Result<()> {
        projections.for_each_column(output, &mut |col_idx, output| match col_idx {
            ProjectedColumn::Data(0) => {
                let mut l_orderkeys = PhysicalI64::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_orderkeys.put(idx, &lineitem.l_orderkey);
                }
                Ok(())
            }
            ProjectedColumn::Data(1) => {
                let mut l_partkeys = PhysicalI64::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_partkeys.put(idx, &lineitem.l_partkey);
                }
                Ok(())
            }
            ProjectedColumn::Data(2) => {
                let mut l_suppkeys = PhysicalI64::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_suppkeys.put(idx, &lineitem.l_suppkey);
                }
                Ok(())
            }
            ProjectedColumn::Data(3) => {
                let mut l_linenumbers = PhysicalI32::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_linenumbers.put(idx, &lineitem.l_linenumber);
                }
                Ok(())
            }
            ProjectedColumn::Data(4) => {
                let mut l_quantities = PhysicalI64::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_quantities.put(idx, &lineitem.l_quantity);
                }
                Ok(())
            }
            ProjectedColumn::Data(5) => {
                let mut l_extended_prices = PhysicalI64::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_extended_prices.put(idx, &lineitem.l_extendedprice.0);
                }
                Ok(())
            }
            ProjectedColumn::Data(6) => {
                let mut l_discounts = PhysicalI64::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_discounts.put(idx, &lineitem.l_discount.0);
                }
                Ok(())
            }
            ProjectedColumn::Data(7) => {
                let mut l_taxes = PhysicalI64::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_taxes.put(idx, &lineitem.l_tax.0);
                }
                Ok(())
            }
            ProjectedColumn::Data(8) => {
                let mut l_return_flags = PhysicalUtf8::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_return_flags.put(idx, lineitem.l_returnflag);
                }
                Ok(())
            }
            ProjectedColumn::Data(9) => {
                let mut l_line_statuses = PhysicalUtf8::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_line_statuses.put(idx, lineitem.l_linestatus);
                }
                Ok(())
            }
            ProjectedColumn::Data(10) => {
                let mut l_ship_dates = PhysicalI32::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_ship_dates.put(
                        idx,
                        &convert::tpch_date_to_days_after_epoch(lineitem.l_shipdate),
                    );
                }
                Ok(())
            }
            ProjectedColumn::Data(11) => {
                let mut l_commit_dates = PhysicalI32::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_commit_dates.put(
                        idx,
                        &convert::tpch_date_to_days_after_epoch(lineitem.l_commitdate),
                    );
                }
                Ok(())
            }
            ProjectedColumn::Data(12) => {
                let mut l_receipt_dates = PhysicalI32::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_receipt_dates.put(
                        idx,
                        &convert::tpch_date_to_days_after_epoch(lineitem.l_receiptdate),
                    );
                }
                Ok(())
            }
            ProjectedColumn::Data(13) => {
                let mut l_shipinstructs = PhysicalUtf8::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_shipinstructs.put(idx, lineitem.l_shipinstruct);
                }
                Ok(())
            }
            ProjectedColumn::Data(14) => {
                let mut l_shipmodes = PhysicalUtf8::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_shipmodes.put(idx, lineitem.l_shipmode);
                }
                Ok(())
            }
            ProjectedColumn::Data(15) => {
                let mut l_comments = PhysicalUtf8::get_addressable_mut(output.data_mut())?;
                for (idx, lineitem) in rows.iter().enumerate() {
                    l_comments.put(idx, lineitem.l_comment);
                }
                Ok(())
            }
            other => panic!("invalid projection {other:?}"),
        })
    }
}
