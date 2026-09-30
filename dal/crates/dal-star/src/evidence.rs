//! Host-issued views and the evidence adapters (§R06 §E06).
//!
//! A [`ReadViewValue`] or [`SearchPageValue`] is a projection of host-owned
//! data: scripts see `path`, `header`, `rows`, and `truncated` but can never
//! mint one. [`show`] wraps such a view in a [`ShowValue`], a pure output
//! descriptor; [`collect_views`] walks a result tree and gathers the intact
//! views inside it for the shared transport's delivery bookkeeping.
//!
//! [`adopt`] implements `ctx.adopt`: it asks the host for an observation
//! reference already eligible to the parent consumer and returns a local
//! view bound to this invocation. It never reads source bytes.
#![expect(unsafe_code, reason = "starlark value derives")]

use std::fmt;
use std::sync::Arc;

use dal_agent::ext::script::{Invocation, ScriptHost};
use dal_core::ext::{
    FindEntry, ReadView, SearchHit, SearchPage, SourceRow, SymbolHit, ToolData, ViewNode,
};
use starlark::any::ProvidesStaticType;
use starlark::values::{Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLike};

use crate::error::api_error;
use crate::outcome::boundary_failure;
use crate::record::{Array, Record};
use crate::value;

/// A host-issued text read view (§R06). Scripts cannot mint one.
///
/// Rows materialize on first access; interned string keys cannot fail
/// hashing, so the projection never reports an error.
#[derive(ProvidesStaticType, Trace, NoSerialize, allocative::Allocative)]
#[repr(C)]
pub(crate) struct ReadViewValue {
    /// The delivered view, including host-only provenance.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    view: ReadView,
}

starlark::starlark_simple_value!(ReadViewValue);

impl fmt::Debug for ReadViewValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for ReadViewValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "read_view({})", self.view.path)
    }
}

#[starlark::values::starlark_value(type = "read_view")]
impl<'v> StarlarkValue<'v> for ReadViewValue {
    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        match attribute {
            "path" => Some(heap.alloc(self.view.path.as_ref())),
            "header" => Some(heap.alloc(self.view.header.as_ref())),
            "truncated" => Some(heap.alloc(self.view.truncated)),
            "rows" => Some(rows_value(heap, &self.view.rows)),
            _ => None,
        }
    }
}

impl ReadViewValue {
    /// Allocates one view value.
    pub(crate) fn alloc(heap: Heap<'_>, view: ReadView) -> Value<'_> {
        heap.alloc(Self { view })
    }

    /// Borrows the delivered view for evidence bookkeeping.
    pub(crate) fn read_view(&self) -> &ReadView {
        &self.view
    }
}

/// A host-issued grep search page (§R06).
#[derive(ProvidesStaticType, Trace, NoSerialize, allocative::Allocative)]
#[repr(C)]
pub(crate) struct SearchPageValue {
    /// The delivered page.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    page: SearchPage,
}

starlark::starlark_simple_value!(SearchPageValue);

impl fmt::Debug for SearchPageValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for SearchPageValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "search_page({} hits)", self.page.matches.len())
    }
}

#[starlark::values::starlark_value(type = "search_page")]
impl<'v> StarlarkValue<'v> for SearchPageValue {
    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        match attribute {
            "truncated" => Some(heap.alloc(self.page.truncated)),
            "matches" => Some(matches_value(heap, &self.page.matches)),
            _ => None,
        }
    }
}

impl SearchPageValue {
    /// Allocates one page value.
    pub(crate) fn alloc(heap: Heap<'_>, page: SearchPage) -> Value<'_> {
        heap.alloc(Self { page })
    }

    /// Borrows the delivered page for evidence bookkeeping.
    pub(crate) fn search_page(&self) -> &SearchPage {
        &self.page
    }
}

/// A `ctx.show(view)` output descriptor (§R06).
///
/// It is not a read and grants nothing: the shared transport rebinds the
/// recorded views for the recipient when the result is actually delivered.
#[derive(ProvidesStaticType, Trace, NoSerialize, allocative::Allocative)]
#[repr(C)]
pub(crate) struct ShowValue {
    /// The wrapped views, as intact as the caller handed them.
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    data: ToolData,
}

starlark::starlark_simple_value!(ShowValue);

impl fmt::Debug for ShowValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for ShowValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("show(..)")
    }
}

#[starlark::values::starlark_value(type = "show")]
#[expect(
    clippy::elidable_lifetime_names,
    reason = "StarlarkValue requires implementations for every value lifetime"
)]
impl<'v> StarlarkValue<'v> for ShowValue {}

impl ShowValue {
    /// Borrows the wrapped views.
    pub(crate) fn tool_data(&self) -> &ToolData {
        &self.data
    }
}

/// Interned string keys cannot fail hashing; record construction is total.
#[expect(
    clippy::expect_used,
    reason = "interned string keys cannot fail Starlark hashing"
)]
fn record_or_empty<'v>(heap: Heap<'v>, fields: Vec<(String, Value<'v>)>) -> Value<'v> {
    Record::alloc(heap, fields).expect("interned string keys always hash")
}

/// Materializes one `SourceRow` list as records `{line, text, complete}`.
fn rows_value<'v>(heap: Heap<'v>, rows: &[SourceRow]) -> Value<'v> {
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(record_or_empty(
            heap,
            vec![
                ("line".into(), heap.alloc(row.line)),
                ("text".into(), heap.alloc(row.text.as_ref())),
                ("complete".into(), heap.alloc(row.complete)),
            ],
        ));
    }
    Array::alloc(heap, items)
}

/// Materializes one `SearchHit` list; each hit carries its intact `source`.
fn matches_value<'v>(heap: Heap<'v>, matches: &[SearchHit]) -> Value<'v> {
    let mut items = Vec::with_capacity(matches.len());
    for hit in matches {
        items.push(record_or_empty(
            heap,
            vec![
                ("path".into(), heap.alloc(hit.path.as_ref())),
                ("line".into(), heap.alloc(hit.line)),
                ("text".into(), heap.alloc(hit.text.as_ref())),
                (
                    "source".into(),
                    ReadViewValue::alloc(heap, hit.source.clone()),
                ),
            ],
        ));
    }
    Array::alloc(heap, items)
}

/// Materializes find entries as `{path, kind}` records.
fn entries_value<'v>(heap: Heap<'v>, entries: &[FindEntry]) -> Value<'v> {
    let mut items = Vec::with_capacity(entries.len());
    for entry in entries {
        items.push(record_or_empty(
            heap,
            vec![
                ("path".into(), heap.alloc(entry.path.as_ref())),
                ("kind".into(), heap.alloc(entry.kind.as_ref())),
            ],
        ));
    }
    Array::alloc(heap, items)
}

/// Materializes symbol hits as `{path, first, last, kind, name}` records.
fn symbols_value<'v>(heap: Heap<'v>, hits: &[SymbolHit]) -> Value<'v> {
    let mut items = Vec::with_capacity(hits.len());
    for hit in hits {
        items.push(record_or_empty(
            heap,
            vec![
                ("path".into(), heap.alloc(hit.path.as_ref())),
                ("first".into(), heap.alloc(hit.first)),
                ("last".into(), heap.alloc(hit.last)),
                ("kind".into(), heap.alloc(hit.kind.as_ref())),
                ("name".into(), heap.alloc(hit.name.as_ref())),
            ],
        ));
    }
    Array::alloc(heap, items)
}

/// Projects one [`ViewNode`] as a tagged record tree (§P06).
fn view_node_value<'v>(node: &ViewNode, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
    match node {
        ViewNode::Text(text) => Ok(record_or_empty(
            heap,
            vec![
                ("type".into(), heap.alloc("text")),
                ("text".into(), heap.alloc(text.as_ref())),
            ],
        )),
        ViewNode::Table { columns, rows } => {
            let column_items = columns
                .iter()
                .map(|column| heap.alloc(column.as_ref()))
                .collect();
            let mut row_items = Vec::with_capacity(rows.len());
            for row in rows {
                let mut cells = Vec::with_capacity(row.len());
                for cell in row {
                    cells.push(
                        value::Value::decode(cell.as_str())
                            .and_then(|value| value.into_starlark(heap))
                            .map_err(|error| api_error(format!("view cell: {error}")))?,
                    );
                }
                row_items.push(Array::alloc(heap, cells));
            }
            Ok(record_or_empty(
                heap,
                vec![
                    ("type".into(), heap.alloc("table")),
                    ("columns".into(), Array::alloc(heap, column_items)),
                    ("rows".into(), Array::alloc(heap, row_items)),
                ],
            ))
        }
        ViewNode::Source(view) => Ok(ReadViewValue::alloc(heap, view.clone())),
        ViewNode::Group(nodes) => {
            let mut items = Vec::with_capacity(nodes.len());
            for node in nodes {
                items.push(view_node_value(node, heap)?);
            }
            Ok(Array::alloc(heap, items))
        }
    }
}

/// One reason a `dal.output` view cannot render (§P06).
#[derive(Debug, thiserror::Error)]
pub(crate) enum ViewError {
    /// The node tree is deeper than the transport depth limit.
    #[error("view nests too deeply")]
    TooDeep,
    /// The value is not a text node, table node, read view, or list.
    #[error("view node must be text, table, a host read view, or a list")]
    UnknownNode,
    /// A text node has no string `text`.
    #[error("text node needs a string `text`")]
    Text,
    /// A table node lacks one list field.
    #[error("table node needs a list `{0}`")]
    TableField(&'static str),
    /// A column label is not a string.
    #[error("column labels must be strings")]
    ColumnLabel,
    /// A row is not a list.
    #[error("each table row must be a list")]
    RowShape,
    /// A row does not have one cell per column.
    #[error("table row has {got} cells; expected {want}")]
    RowWidth {
        /// The cells in the row.
        got: usize,
        /// The number of columns.
        want: usize,
    },
    /// A cell is a list or object.
    #[error("table cells must be scalar values")]
    NonScalarCell,
    /// A cell is outside the transport set.
    #[error(transparent)]
    Codec(#[from] value::CodecError),
    /// A cell did not encode as JSON.
    #[error(transparent)]
    Json(#[from] dal_core::RawJsonError),
}

/// Decodes a `dal.output` view into a [`ViewNode`] (§P06).
///
/// This is the inverse of [`view_node_value`]: a mapping with `type` equal to
/// `"text"` or `"table"`, an intact host read view, or a list of nodes. Table
/// cells must be scalar transport values and every row must have one cell
/// per column; nothing else renders.
pub(crate) fn view_node_from(value: Value<'_>) -> Result<ViewNode, ViewError> {
    view_node_at(value, 0)
}

/// Decodes one node at `depth`, bounded by the transport depth limit.
fn view_node_at(value: Value<'_>, depth: usize) -> Result<ViewNode, ViewError> {
    if depth > value::MAX_VALUE_DEPTH {
        return Err(ViewError::TooDeep);
    }
    if let Some(read) = ReadViewValue::from_value(value) {
        return Ok(ViewNode::Source(read.read_view().clone()));
    }
    if let Some(items) = sequence(value) {
        return items
            .into_iter()
            .map(|item| view_node_at(item, depth + 1))
            .collect::<Result<_, _>>()
            .map(ViewNode::Group);
    }
    match field(value, "type").and_then(Value::unpack_str) {
        Some("text") => text_node(value),
        Some("table") => table_node(value),
        _ => Err(ViewError::UnknownNode),
    }
}

/// Decodes a `{type: "text", text}` node.
fn text_node(value: Value<'_>) -> Result<ViewNode, ViewError> {
    field(value, "text")
        .and_then(Value::unpack_str)
        .map(|text| ViewNode::Text(text.into()))
        .ok_or(ViewError::Text)
}

/// Decodes a `{type: "table", columns, rows}` node.
fn table_node(value: Value<'_>) -> Result<ViewNode, ViewError> {
    let columns = field(value, "columns")
        .and_then(sequence)
        .ok_or(ViewError::TableField("columns"))?
        .into_iter()
        .map(|column| {
            column
                .unpack_str()
                .map(Box::from)
                .ok_or(ViewError::ColumnLabel)
        })
        .collect::<Result<Box<[Box<str>]>, _>>()?;
    let rows = field(value, "rows")
        .and_then(sequence)
        .ok_or(ViewError::TableField("rows"))?
        .into_iter()
        .map(|row| table_row(row, columns.len()))
        .collect::<Result<_, _>>()?;
    Ok(ViewNode::Table { columns, rows })
}

/// Decodes one table row of exactly `width` scalar cells.
fn table_row(row: Value<'_>, width: usize) -> Result<Box<[dal_core::RawJson]>, ViewError> {
    let cells = sequence(row).ok_or(ViewError::RowShape)?;
    if cells.len() != width {
        return Err(ViewError::RowWidth {
            got: cells.len(),
            want: width,
        });
    }
    cells.into_iter().map(scalar_cell).collect()
}

/// Encodes one scalar cell as raw JSON.
fn scalar_cell(cell: Value<'_>) -> Result<dal_core::RawJson, ViewError> {
    let cell = value::Value::from_starlark(cell)?;
    if matches!(cell, value::Value::List(_) | value::Value::Object(_)) {
        return Err(ViewError::NonScalarCell);
    }
    Ok(dal_core::RawJson::parse(&cell.to_json())?)
}

/// The items of a list, tuple, or host array; `None` for anything else.
fn sequence(value: Value<'_>) -> Option<Vec<Value<'_>>> {
    if let Some(array) = Array::from_value(value) {
        return Some(array.items().to_vec());
    }
    if let Some(list) = starlark::values::list::ListRef::from_value(value) {
        return Some(list.iter().collect());
    }
    starlark::values::tuple::TupleRef::from_value(value).map(|tuple| tuple.iter().collect())
}

/// One named field of a dict with string keys or a host record.
fn field<'v>(value: Value<'v>, name: &str) -> Option<Value<'v>> {
    if let Some(dict) = starlark::values::dict::DictRef::from_value(value) {
        return dict.get_str(name);
    }
    Record::from_value(value)?
        .iter()
        .find(|(key, _)| key.unpack_str() == Some(name))
        .map(|(_, item)| item)
}

/// Projects one [`ToolData`] into the caller's heap (§R06).
pub(crate) fn tool_data_value(data: ToolData, heap: Heap<'_>) -> starlark::Result<Value<'_>> {
    match data {
        ToolData::Read(view) => Ok(ReadViewValue::alloc(heap, view)),
        ToolData::Search(page) => Ok(SearchPageValue::alloc(heap, page)),
        ToolData::Find(page) => Ok(record_or_empty(
            heap,
            vec![
                ("entries".into(), entries_value(heap, &page.entries)),
                ("truncated".into(), heap.alloc(page.truncated)),
            ],
        )),
        ToolData::Symbols(page) => Ok(record_or_empty(
            heap,
            vec![
                ("hits".into(), symbols_value(heap, &page.hits)),
                ("truncated".into(), heap.alloc(page.truncated)),
            ],
        )),
        ToolData::Views(views) => {
            let items = views
                .into_vec()
                .into_iter()
                .map(|view| ReadViewValue::alloc(heap, view))
                .collect();
            Ok(record_or_empty(
                heap,
                vec![("views".into(), Array::alloc(heap, items))],
            ))
        }
        ToolData::Display(node) => view_node_value(&node, heap),
    }
}

/// Implements `ctx.show(view)` (§R06).
///
/// Only an intact host-issued view is accepted: copied `{path,line,text}`
/// dictionaries carry no evidence and are refused.
pub(crate) fn show<'v>(heap: Heap<'v>, view: Value<'v>) -> starlark::Result<Value<'v>> {
    if let Some(read) = ReadViewValue::from_value(view) {
        return Ok(heap.alloc(ShowValue {
            data: ToolData::Read(read.read_view().clone()),
        }));
    }
    if let Some(search) = SearchPageValue::from_value(view) {
        return Ok(heap.alloc(ShowValue {
            data: ToolData::Search(search.search_page().clone()),
        }));
    }
    Err(api_error(
        "ctx.show: only an intact host-issued read view or search page is accepted",
    ))
}

/// Implements `ctx.adopt(reference)` (§E06).
///
/// The host owns eligibility; a refused reference raises the recoverable
/// `observation_unavailable` boundary failure.
pub(crate) fn adopt<'v>(
    heap: Heap<'v>,
    inv: &Arc<Invocation>,
    host: &Arc<dyn ScriptHost>,
    reference: &str,
) -> starlark::Result<Value<'v>> {
    let view = host.adopt(inv, reference).map_err(boundary_failure)?;
    Ok(ReadViewValue::alloc(heap, view))
}

/// Gathers the intact views a result tree carries (§R06 §E06).
///
/// Read views, search pages, and `ctx.show` descriptors contribute their
/// delivered rows. Records, arrays, lists, tuples, and dicts are traversed;
/// copied dictionaries contribute nothing.
pub(crate) fn collect_views(value: Value<'_>, out: &mut Vec<ReadView>) {
    if let Some(read) = ReadViewValue::from_value(value) {
        out.push(read.read_view().clone());
        return;
    }
    if let Some(search) = SearchPageValue::from_value(value) {
        out.extend(
            search
                .search_page()
                .matches
                .iter()
                .map(|hit| hit.source.clone()),
        );
        return;
    }
    if let Some(shown) = ValueLike::downcast_ref::<ShowValue>(value) {
        collect_shown(shown.tool_data(), out);
        return;
    }
    if let Some(record) = Record::from_value(value) {
        for (_, item) in record.iter() {
            collect_views(item, out);
        }
        return;
    }
    if let Some(array) = Array::from_value(value) {
        for item in array.items() {
            collect_views(*item, out);
        }
        return;
    }
    if let Some(list) = starlark::values::list::ListRef::from_value(value) {
        for item in list.iter() {
            collect_views(item, out);
        }
        return;
    }
    if let Some(tuple) = starlark::values::tuple::TupleRef::from_value(value) {
        for item in tuple.iter() {
            collect_views(item, out);
        }
        return;
    }
    if let Some(dict) = starlark::values::dict::DictRef::from_value(value) {
        for (_, item) in dict.iter() {
            collect_views(item, out);
        }
    }
}

/// Contributes the views a [`ShowValue`] wraps.
fn collect_shown(data: &ToolData, out: &mut Vec<ReadView>) {
    match data {
        ToolData::Read(view) => out.push(view.clone()),
        ToolData::Search(page) => {
            out.extend(page.matches.iter().map(|hit| hit.source.clone()));
        }
        _ => {}
    }
}
