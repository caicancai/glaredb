use std::fmt::Debug;

use glaredb_core::arrays::array::Array;
use glaredb_core::arrays::datatype::DataType;
use glaredb_core::buffer::buffer_manager::AsRawBufferManager;
use glaredb_error::{DbError, Result};

use super::page_reader::PageReader;
use super::row_group_pruner::{PlainType, RowGroupPruner};
use super::value_reader::ValueReader;
use crate::basic::Compression;
use crate::column::encoding::{Definitions, PageDecoder};
use crate::compression::{CodecOptions, create_codec};
use crate::metadata::statistics::Statistics;
use crate::schema::types::ColumnDescriptor;

/// Reads column values into the output array.
///
/// This trait exists entirely for boxing `ValueColumnReader`.
pub trait ColumnReader: Debug + Sync + Send {
    /// Reads `count` values into the output array.
    fn read(&mut self, output: &mut Array, count: usize) -> Result<()>;

    /// Prepares this reader for reading from a new "scan unit" by updating the
    /// column descriptor to use.
    ///
    /// Errors if the physical type of the new descriptor does not match the
    /// physical type of the existing descriptor.
    fn prepare_scan_unit(&mut self, descr: ColumnDescriptor) -> Result<()>;

    /// Prepares this reader for the next chunk.
    ///
    /// The underlying chunk buffers should be resized to accomadate the new
    /// chunk.
    fn prepare_for_chunk(&mut self, chunk_size: usize, compression: Compression) -> Result<()>;

    /// Return a mutable reference to the chunk buffer.
    ///
    /// This buffer will be used for reading the column chunk from the file
    /// directly.
    fn chunk_buf_mut(&mut self) -> &mut [u8];

    /// See if we can prune a row group based on these column statistics.
    fn should_prune(&self, stats: &Statistics) -> Result<bool>;
}

#[derive(Debug)]
pub struct ValueColumnReader<V: ValueReader, F: RowGroupPruner<V::PlainType>> {
    /// Filter used for pruning row groups.
    pub(crate) filter: F,
    /// Page reader for this column.
    pub(crate) page_reader: PageReader<V>,
    /// Reusable buffer for definition levels.
    pub(crate) definitions: Vec<i16>,
    /// Reusable buffer for repetition levels.
    pub(crate) repetitions: Vec<i16>,
}

impl<V, F> ValueColumnReader<V, F>
where
    V: ValueReader,
    F: RowGroupPruner<V::PlainType>,
{
    pub fn try_new(
        manager: &impl AsRawBufferManager,
        datatype: DataType,
        descr: ColumnDescriptor,
        filter: F,
    ) -> Result<Self> {
        let page_reader = PageReader::try_new(manager, datatype, descr)?;

        Ok(ValueColumnReader {
            filter,
            page_reader,
            definitions: Vec::new(),
            repetitions: Vec::new(),
        })
    }
}

impl<V, F> ColumnReader for ValueColumnReader<V, F>
where
    V: ValueReader,
    F: RowGroupPruner<V::PlainType>,
{
    fn prepare_for_chunk(&mut self, chunk_size: usize, compression: Compression) -> Result<()> {
        self.page_reader.chunk_offset = 0;
        // SAFETY: We'll be writing `chunk_size` bytes to this chunks before
        // reading it.
        unsafe { self.page_reader.chunk.resize_uninit(chunk_size)? };
        self.page_reader.codec = create_codec(compression, &CodecOptions::default())?;
        Ok(())
    }

    fn prepare_scan_unit(&mut self, descr: ColumnDescriptor) -> Result<()> {
        self.page_reader.prepare_scan_unit(descr)
    }

    fn chunk_buf_mut(&mut self) -> &mut [u8] {
        self.page_reader.chunk.as_slice_mut()
    }

    fn read(&mut self, output: &mut Array, count: usize) -> Result<()> {
        let mut offset = 0;
        let mut remaining = count;

        // Resize each buffer. The current values don't matter as they'll be
        // overwritten or not read at all.
        //
        // These are resized to the max size they can. In the below loop, we
        // slice them down to the required sizes.
        self.definitions.resize(count, 0);
        self.repetitions.resize(count, 0);

        while remaining > 0 {
            if self.page_reader.state.remaining_page_values == 0 {
                // Read next page.
                self.page_reader.prepare_next()?;
                // Continue, in case this page contains no values.
                continue;
            }

            // Number of records we're going to be reading from this page.
            let count = usize::min(remaining, self.page_reader.state.remaining_page_values);

            // Read in repetitions/definitions. Ensure slices are exact-sized.
            let definitions = &mut self.definitions[..count];
            let repetitions = &mut self.repetitions[..count];
            self.page_reader.read_levels(definitions, repetitions)?;

            let definitions = if self.page_reader.state.definitions.is_some() {
                // Use the sliced definititions.
                Definitions::HasDefinitions {
                    levels: definitions,
                    max: self.page_reader.descr.max_def_level,
                }
            } else {
                Definitions::NoDefinitions
            };

            // Read the actual data.
            let decoder = match self.page_reader.state.page_decoder.as_mut() {
                Some(decoder) => decoder,
                None => return Err(DbError::new("Missing page decoder")),
            };
            match decoder {
                PageDecoder::Plain(dec) => dec.read_plain(definitions, output, offset, count)?,
                PageDecoder::Dictionary(dec) => dec.read(
                    &self.page_reader.state.dictionary,
                    definitions,
                    output,
                    offset,
                    count,
                )?,
                PageDecoder::DeltaBinaryPackedI32(dec) => {
                    dec.read(definitions, output, offset, count)?
                }
                PageDecoder::DeltaBinaryPackedI64(dec) => {
                    dec.read(definitions, output, offset, count)?
                }
                PageDecoder::DeltaLengthByteArray(dec) => {
                    dec.read(definitions, output, offset, count)?
                }
                PageDecoder::DeltaByteArray(dec) => dec.read(definitions, output, offset, count)?,
                PageDecoder::RleBool(dec) => dec.read(definitions, output, offset, count)?,
                PageDecoder::ByteStreamSplit4(dec) => {
                    dec.read(definitions, output, offset, count)?
                }
                PageDecoder::ByteStreamSplit8(dec) => {
                    dec.read(definitions, output, offset, count)?
                }
            }

            // Update page reader state.
            self.page_reader.state.remaining_page_values -= count;

            offset += count;
            remaining -= count;
        }

        Ok(())
    }

    fn should_prune(&self, stats: &Statistics) -> Result<bool> {
        let stats = <V::PlainType as PlainType>::statistics(stats)
            .ok_or_else(|| DbError::new("Unexpected column stats"))?;
        self.filter.should_prune(stats)
    }
}
