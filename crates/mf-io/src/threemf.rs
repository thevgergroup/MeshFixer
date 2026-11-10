//! 3MF core (MF-IO-001, MF-DOC-004): mesh objects, build-item transforms and
//! the model unit. Unsupported features (components, extensions) produce
//! warnings rather than silent loss.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};

use mf_geometry::{Material, Mesh, NO_MATERIAL};
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use serde::{Deserialize, Serialize};

use mf_runtime::{RunContext, TICK_INTERVAL};

use crate::{IoError, Limits, PARSE_FROM, READING, significant_digits};

/// 3MF model units. 3MF defaults to millimetres.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Unit {
    Micron,
    #[default]
    Millimeter,
    Centimeter,
    Inch,
    Foot,
    Meter,
}

impl Unit {
    fn from_attr(s: &str) -> Option<Unit> {
        Some(match s {
            "micron" => Unit::Micron,
            "millimeter" => Unit::Millimeter,
            "centimeter" => Unit::Centimeter,
            "inch" => Unit::Inch,
            "foot" => Unit::Foot,
            "meter" => Unit::Meter,
            _ => return None,
        })
    }

    fn as_attr(&self) -> &'static str {
        match self {
            Unit::Micron => "micron",
            Unit::Millimeter => "millimeter",
            Unit::Centimeter => "centimeter",
            Unit::Inch => "inch",
            Unit::Foot => "foot",
            Unit::Meter => "meter",
        }
    }
}

/// Row-major 3×4 affine in 3MF order: `m00 m01 m02 m10 m11 m12 m20 m21 m22 m30 m31 m32`
/// (the last three are the translation).
pub const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

#[derive(Debug, Clone)]
pub struct ModelObject {
    pub name: String,
    pub mesh: Mesh,
    pub transform: [f64; 12],
    /// The object's `<metadata>` (in its `<metadatagroup>`): name to
    /// value. MeshFixer's own entries are named `meshfixer:<key>` (see
    /// [`MESHFIXER_NS`]), such as `meshfixer:role` = `hole`.
    pub metadata: BTreeMap<String, String>,
}

/// The namespace of MeshFixer's own object metadata names
/// (`meshfixer:<key>`), declared on `<model>` when an object has any.
pub const MESHFIXER_NS: &str = "https://thevgergroup.com/meshfixer/3mf/2026";

#[derive(Debug, Clone)]
pub struct Model {
    pub unit: Unit,
    pub objects: Vec<ModelObject>,
}

const CORE_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
/// The Materials and Properties extension (colour groups).
const MATERIAL_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/material/2015/02";
const MODEL_REL: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";
const DEFAULT_MODEL_PATH: &str = "3D/3dmodel.model";

fn not_3mf(message: String) -> IoError {
    IoError::Parse { line: 0, message }
}

fn parse_u32(raw: &str, name: &str) -> Result<u32, String> {
    raw.trim()
        .parse::<u32>()
        .map_err(|_| format!("{name}=\"{raw}\" is not a whole number"))
}

/// 1-based line of byte offsets into `text`, for offsets that only grow:
/// each call counts newlines since the previous one, so a whole read stays
/// linear in the file size.
struct LineCounter<'a> {
    text: &'a [u8],
    offset: usize,
    line: usize,
}

impl<'a> LineCounter<'a> {
    fn new(text: &'a str) -> Self {
        LineCounter {
            text: text.as_bytes(),
            offset: 0,
            line: 1,
        }
    }

    fn line_at(&mut self, offset: u64) -> usize {
        let offset = (offset as usize).min(self.text.len());
        debug_assert!(offset >= self.offset, "offsets must not decrease");
        self.line += self.text[self.offset..offset]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        self.offset = offset;
        self.line
    }
}

/// Prefixes the `<model>` element binds to the core namespace (`None` for
/// the default namespace). With no default namespace declared, unprefixed
/// elements are taken as core.
fn core_namespace_prefixes(e: &BytesStart) -> Result<BTreeSet<Option<String>>, String> {
    namespace_prefixes(e, CORE_NS, true)
}

/// The prefixes bound to `ns` on `e` (`None`: the default namespace).
/// With `default_if_undeclared`, an element with no default namespace
/// declaration counts its unprefixed names as `ns`.
fn namespace_prefixes(
    e: &BytesStart,
    ns: &str,
    default_if_undeclared: bool,
) -> Result<BTreeSet<Option<String>>, String> {
    let mut core = BTreeSet::new();
    let mut default_declared = false;
    for a in e.attributes() {
        let a = a.map_err(|err| format!("malformed attribute: {err}"))?;
        let key = a.key.as_ref().to_owned();
        let prefix = if key == "xmlns" {
            default_declared = true;
            None
        } else if let Some(p) = key.strip_prefix("xmlns:") {
            Some(p.to_owned())
        } else {
            continue;
        };
        let uri = a
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|err| format!("namespace {key} has an invalid value: {err}"))?;
        if uri == ns {
            core.insert(prefix);
        }
    }
    if !default_declared && default_if_undeclared {
        core.insert(None);
    }
    Ok(core)
}

/// An attribute's decoded value. A malformed attribute or value is an error
/// naming it, never treated as absent (which would silently apply defaults).
fn attr(e: &BytesStart, name: &str) -> Result<Option<String>, String> {
    for a in e.attributes() {
        let a = a.map_err(|err| format!("malformed attribute: {err}"))?;
        if a.key.local_name().as_ref() == name {
            let v = a
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|err| format!("attribute {name} has an invalid value: {err}"))?;
            return Ok(Some(v.into_owned()));
        }
    }
    Ok(None)
}

/// Read a 3MF archive. Returns the model, the largest number of significant
/// digits seen in coordinates, and warnings.
pub fn read_with_precision(bytes: &[u8]) -> Result<(Model, u8, Vec<String>), IoError> {
    read_with(bytes, &RunContext::none())
}

/// Where in `"reading"` decompressing the model part starts and XML parsing
/// starts (reading the file comes first).
const UNZIP_FROM: f64 = PARSE_FROM;
const XML_FROM: f64 = 0.7;

/// As [`read_with_precision`], cancellable, reporting stage `"reading"` by
/// bytes decompressed and then parsed.
pub fn read_with(bytes: &[u8], ctx: &RunContext) -> Result<(Model, u8, Vec<String>), IoError> {
    read_with_limits(bytes, ctx, Limits::default())
}

/// As [`read_with`], with explicit [`Limits`] (tests use tighter ones than
/// the defaults so hostile-input tests don't need gigabyte fixtures).
pub fn read_with_limits(
    bytes: &[u8],
    ctx: &RunContext,
    limits: Limits,
) -> Result<(Model, u8, Vec<String>), IoError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| not_3mf(format!("not a 3MF (zip) archive: {e}")))?;
    let mut warnings = Vec::new();
    // Decompressed in blocks so a cancel is seen between them; only the
    // model part (`progress`) is large enough to report.
    //
    // Bounds are checked twice, both before any inflate buffer grows large
    // (MF-IO-002, review focus 5):
    //  - the entry's declared uncompressed size and its ratio to the
    //    compressed size on disk, against `Limits`, before reading a byte;
    //  - a hard stop on bytes actually decompressed, in case the declared
    //    size understates reality (a corrupt or adversarial local header).
    // Either way, a zip bomb fails fast with `LimitExceeded`, never an
    // unbounded inflate.
    let read_entry = |zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
                      name: &str,
                      progress: bool|
     -> Result<String, IoError> {
        const BLOCK: u64 = 4 << 20;
        let mut entry = zip
            .by_name(name)
            .map_err(|e| not_3mf(format!("3MF archive has no '{name}': {e}")))?;
        let declared = entry.size();
        let compressed = entry.compressed_size();
        if declared > limits.max_model_part_bytes {
            return Err(IoError::LimitExceeded {
                what: "declared 3MF archive entry size",
                limit: limits.max_model_part_bytes,
                found: declared,
            });
        }
        // Only meaningful once there is enough compressed data that a
        // ratio says anything (a few bytes of input can trivially "ratio"
        // high on tiny, legitimate entries).
        if compressed >= 64 && declared / compressed > limits.max_compression_ratio {
            return Err(IoError::LimitExceeded {
                what: "3MF archive entry compression ratio",
                limit: limits.max_compression_ratio,
                found: declared / compressed,
            });
        }
        let size = declared as usize;
        let span = progress.then(|| ctx.span(READING, UNZIP_FROM, XML_FROM, size));
        let mut buf = Vec::new();
        loop {
            match &span {
                Some(span) => span.step(buf.len())?,
                None => ctx.checkpoint()?,
            }
            let read = (&mut entry)
                .take(BLOCK)
                .read_to_end(&mut buf)
                .map_err(|e| not_3mf(format!("'{name}' is not UTF-8 text: {e}")))?;
            if read == 0 {
                break;
            }
            // Hard stop on what was actually decompressed, independent of
            // what the entry's header claimed: a lying declared size must
            // not let actual inflate output grow past the limit either.
            if buf.len() as u64 > limits.max_model_part_bytes {
                return Err(IoError::LimitExceeded {
                    what: "decompressed 3MF archive entry size",
                    limit: limits.max_model_part_bytes,
                    found: buf.len() as u64,
                });
            }
        }
        String::from_utf8(buf).map_err(|e| not_3mf(format!("'{name}' is not UTF-8 text: {e}")))
    };

    // Locate the model part through the package relationships. Only a
    // _rels/.rels entry that is genuinely absent from the archive falls
    // back to the standard path (with a warning); one that is present but
    // unreadable or corrupt (a bad size/ratio, invalid UTF-8, malformed
    // XML, or missing the model relationship) is a named error, never a
    // silent fallback (MF-ISSUE-039).
    let model_path = if zip.by_name("_rels/.rels").is_err() {
        warnings.push(format!(
            "3MF archive has no _rels/.rels; using the standard path {DEFAULT_MODEL_PATH}"
        ));
        DEFAULT_MODEL_PATH.to_owned()
    } else {
        let rels = read_entry(&mut zip, "_rels/.rels", false)
            .map_err(|e| not_3mf(format!("_rels/.rels is present but unreadable: {e}")))?;
        let mut reader = quick_xml::Reader::from_str(&rels);
        let mut target = None;
        loop {
            match reader.read_event() {
                Ok(Event::Empty(e)) | Ok(Event::Start(e))
                    if e.local_name().as_ref() == "Relationship" =>
                {
                    if attr(&e, "Type").map_err(not_3mf)?.as_deref() == Some(MODEL_REL) {
                        target = attr(&e, "Target").map_err(not_3mf)?;
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(not_3mf(format!("_rels/.rels: {e}"))),
                _ => {}
            }
        }
        target
            .map(|t| t.trim_start_matches('/').to_owned())
            .ok_or_else(|| not_3mf("_rels/.rels has no 3D model relationship".into()))?
    };
    let text = read_entry(&mut zip, &model_path, true)?;

    #[derive(Default)]
    struct Obj {
        id: u32,
        name: Option<String>,
        positions: Vec<[f64; 3]>,
        triangles: Vec<[u32; 3]>,
        has_mesh: bool,
        has_components: bool,
        line: usize,
        /// The object's default property: (pid, pindex).
        property: Option<(u32, u32)>,
        /// Per triangle: its own (pid, p1), if it names one.
        triangle_properties: Vec<Option<(u32, u32)>>,
        /// Its `<metadatagroup>`'s entries.
        metadata: BTreeMap<String, String>,
    }
    // The object metadata entry being read: its name and text so far.
    let mut open_metadata: Option<(String, String)> = None;
    let mut unit = Unit::Millimeter;
    let mut objects: Vec<Obj> = Vec::new();
    let mut current: Option<Obj> = None;
    let mut items: Vec<(u32, [f64; 12], usize)> = Vec::new();
    let mut saw_build = false;
    let mut digits = 0u8;
    // Namespace prefixes bound to the core namespace (`None` is the default
    // namespace). Elements in any other namespace are extensions: skipped
    // and named in a warning, never parsed as core elements.
    let mut core_prefixes: BTreeSet<Option<String>> = BTreeSet::from([None]);
    let mut ignored_elements: BTreeSet<String> = BTreeSet::new();
    let mut material_prefixes: BTreeSet<Option<String>> = BTreeSet::new();
    // Property groups by id: colours and materials (MF-ISSUE-091).
    let mut groups: BTreeMap<u32, Vec<Material>> = BTreeMap::new();
    let mut open_group: Option<u32> = None;
    // Property resources MeshFixer doesn't import (textures, composites).
    let mut unsupported_groups: BTreeSet<u32> = BTreeSet::new();
    let mut gradient_triangles = 0usize;
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut lines = LineCounter::new(&text);
    let xml = ctx.span(READING, XML_FROM, 1.0, text.len());
    let mut events = 0usize;
    loop {
        if events.is_multiple_of(TICK_INTERVAL as usize) {
            xml.step(reader.buffer_position() as usize)?;
        }
        events += 1;
        let pos = reader.buffer_position();
        let line = lines.line_at(pos);
        let event = reader.read_event().map_err(|e| IoError::Parse {
            line,
            message: format!("3MF model XML: {e}"),
        })?;
        let parse_err = |message: String| IoError::Parse { line, message };
        let number = |e: &BytesStart, name: &str, digits: &mut u8| -> Result<f64, IoError> {
            let raw = attr(e, name).map_err(parse_err)?.ok_or_else(|| {
                parse_err(format!(
                    "<{}> is missing {}",
                    e.local_name().as_ref().to_owned(),
                    name
                ))
            })?;
            *digits = (*digits).max(significant_digits(&raw));
            raw.trim()
                .parse::<f64>()
                .map_err(|_| parse_err(format!("{}=\"{raw}\" is not a number", name)))
        };
        let index = |e: &BytesStart, name: &str| -> Result<u32, IoError> {
            let raw = attr(e, name)
                .map_err(parse_err)?
                .ok_or_else(|| parse_err(format!("<triangle> is missing {}", name)))?;
            raw.trim()
                .parse::<u32>()
                .map_err(|_| parse_err(format!("{}=\"{raw}\" is not a vertex index", name)))
        };
        let (e, is_empty) = match event {
            Event::Start(e) => (Some(e), false),
            Event::Empty(e) => (Some(e), true),
            Event::Text(t) => {
                if let Some((_, value)) = open_metadata.as_mut() {
                    value.push_str(&t.xml10_content());
                }
                (None, false)
            }
            Event::GeneralRef(r) => {
                if let Some((_, value)) = open_metadata.as_mut() {
                    let name = r.xml10_content();
                    let raw = format!("&{name};");
                    let text = quick_xml::escape::unescape(&raw)
                        .map_err(|e| parse_err(format!("<metadata> text: {e}")))?;
                    value.push_str(&text);
                }
                (None, false)
            }
            Event::End(end) => {
                if matches!(end.local_name().as_ref(), "basematerials" | "colorgroup") {
                    open_group = None;
                }
                if end.local_name().as_ref() == "metadata"
                    && let Some((name, value)) = open_metadata.take()
                    && let Some(o) = current.as_mut()
                {
                    o.metadata.insert(name, value);
                }
                if end.local_name().as_ref() == "object"
                    && let Some(o) = current.take()
                {
                    objects.push(o);
                }
                (None, false)
            }
            Event::Eof => break,
            _ => (None, false),
        };
        if let Some(e) = e {
            if e.local_name().as_ref() == "model" {
                core_prefixes = core_namespace_prefixes(&e).map_err(parse_err)?;
                material_prefixes =
                    namespace_prefixes(&e, MATERIAL_NS, false).map_err(parse_err)?;
            }
            let prefix = e.name().prefix().map(|p| p.as_ref().to_owned());
            if material_prefixes.contains(&prefix) {
                match e.local_name().as_ref() {
                    "colorgroup" => {
                        let id = attr(&e, "id")
                            .map_err(parse_err)?
                            // allow-swallow: a non-numeric id becomes the named error on the next line
                            .and_then(|v| v.trim().parse().ok())
                            .ok_or_else(|| parse_err("<colorgroup> needs a numeric id".into()))?;
                        groups.insert(id, Vec::new());
                        open_group = (!is_empty).then_some(id);
                    }
                    "color" => {
                        let raw = attr(&e, "color")
                            .map_err(parse_err)?
                            .ok_or_else(|| parse_err("<color> is missing color".into()))?;
                        let color = Material::parse_color(&raw).ok_or_else(|| {
                            parse_err(format!("color=\"{raw}\" is not #RRGGBB[AA]"))
                        })?;
                        let group = open_group
                            .and_then(|g| groups.get_mut(&g))
                            .ok_or_else(|| parse_err("<color> outside a <colorgroup>".into()))?;
                        group.push(Material {
                            name: String::new(),
                            color,
                        });
                    }
                    _ => {
                        if let Some(id) = attr(&e, "id")
                            .map_err(parse_err)?
                            // allow-swallow: only used to name the group in a warning
                            .and_then(|v| v.trim().parse().ok())
                        {
                            unsupported_groups.insert(id);
                        }
                        ignored_elements.insert(e.name().as_ref().to_owned());
                    }
                }
                continue;
            }
            if !core_prefixes.contains(&prefix) {
                ignored_elements.insert(e.name().as_ref().to_owned());
                continue;
            }
            match e.local_name().as_ref() {
                "model" => {
                    if let Some(req) = attr(&e, "requiredextensions").map_err(parse_err)? {
                        warnings.push(format!(
                            "3MF model requires extension(s) '{req}', which MeshFixer doesn't support; only core geometry was imported"
                        ));
                    }
                    if let Some(u) = attr(&e, "unit").map_err(parse_err)? {
                        unit = Unit::from_attr(&u)
                            .ok_or_else(|| parse_err(format!("unknown 3MF unit '{u}'")))?;
                    }
                }
                "object" => {
                    let id = attr(&e, "id")
                        .map_err(parse_err)?
                        // allow-swallow: a non-numeric id becomes the named error on the next line
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| parse_err("<object> needs a numeric id".into()))?;
                    if let Some(t) = attr(&e, "type").map_err(parse_err)?
                        && t != "model"
                    {
                        warnings.push(format!(
                            "object {id} has type '{t}'; MeshFixer imports it as an ordinary model"
                        ));
                    }
                    let property = match attr(&e, "pid").map_err(parse_err)? {
                        None => None,
                        Some(pid) => Some((
                            parse_u32(&pid, "pid").map_err(parse_err)?,
                            match attr(&e, "pindex").map_err(parse_err)? {
                                Some(i) => parse_u32(&i, "pindex").map_err(parse_err)?,
                                None => 0,
                            },
                        )),
                    };
                    let obj = Obj {
                        id,
                        name: attr(&e, "name").map_err(parse_err)?,
                        line,
                        property,
                        ..Obj::default()
                    };
                    if is_empty {
                        objects.push(obj);
                    } else {
                        current = Some(obj);
                    }
                }
                "mesh" => {
                    if let Some(o) = current.as_mut() {
                        o.has_mesh = true;
                    }
                }
                "vertex" => {
                    let p = [
                        number(&e, "x", &mut digits)?,
                        number(&e, "y", &mut digits)?,
                        number(&e, "z", &mut digits)?,
                    ];
                    current
                        .as_mut()
                        .ok_or_else(|| parse_err("<vertex> outside an object".into()))?
                        .positions
                        .push(p);
                }
                "triangle" => {
                    let t = [index(&e, "v1")?, index(&e, "v2")?, index(&e, "v3")?];
                    let get = |name: &str| -> Result<Option<u32>, IoError> {
                        match attr(&e, name).map_err(parse_err)? {
                            None => Ok(None),
                            Some(v) => parse_u32(&v, name).map(Some).map_err(parse_err),
                        }
                    };
                    let (pid, p1, p2, p3) = (get("pid")?, get("p1")?, get("p2")?, get("p3")?);
                    let o = current
                        .as_mut()
                        .ok_or_else(|| parse_err("<triangle> outside an object".into()))?;
                    // A triangle names its own property with p1 (and its
                    // own pid, or the object's); one with neither uses the
                    // object's default.
                    let own = match (pid, p1) {
                        (None, None) => None,
                        (pid, p1) => {
                            let group = pid.or(o.property.map(|p| p.0)).ok_or_else(|| {
                                parse_err(
                                    "<triangle> has p1 but neither it nor its object has a pid"
                                        .into(),
                                )
                            })?;
                            // allow-swallow: 3MF: a triangle pid without p1 means entry 0 of that group
                            Some((group, p1.or(o.property.map(|p| p.1)).unwrap_or(0)))
                        }
                    };
                    if own.is_some()
                        && (p2.is_some_and(|p| Some(p) != p1) || p3.is_some_and(|p| Some(p) != p1))
                    {
                        gradient_triangles += 1;
                    }
                    o.triangles.push(t);
                    o.triangle_properties.push(own);
                }
                "metadata" if current.is_some() => {
                    let name = attr(&e, "name")
                        .map_err(parse_err)?
                        .ok_or_else(|| parse_err("<metadata> is missing name".into()))?;
                    if is_empty {
                        if let Some(o) = current.as_mut() {
                            o.metadata.insert(name, String::new());
                        }
                    } else {
                        open_metadata = Some((name, String::new()));
                    }
                }
                "components" => {
                    if let Some(o) = current.as_mut() {
                        o.has_components = true;
                    }
                }
                "basematerials" => {
                    let id = attr(&e, "id")
                        .map_err(parse_err)?
                        // allow-swallow: a non-numeric id becomes the named error on the next line
                        .and_then(|v| v.trim().parse().ok())
                        .ok_or_else(|| parse_err("<basematerials> needs a numeric id".into()))?;
                    groups.insert(id, Vec::new());
                    open_group = (!is_empty).then_some(id);
                }
                "base" => {
                    // allow-swallow: a nameless base material is still a colour; its name stays empty
                    let name = attr(&e, "name").map_err(parse_err)?.unwrap_or_default();
                    let raw = attr(&e, "displaycolor")
                        .map_err(parse_err)?
                        .ok_or_else(|| parse_err("<base> is missing displaycolor".into()))?;
                    let color = Material::parse_color(&raw).ok_or_else(|| {
                        parse_err(format!("displaycolor=\"{raw}\" is not #RRGGBB[AA]"))
                    })?;
                    let group = open_group
                        .and_then(|g| groups.get_mut(&g))
                        .ok_or_else(|| parse_err("<base> outside <basematerials>".into()))?;
                    group.push(Material { name, color });
                }
                "build" => saw_build = true,
                "item" => {
                    let id = attr(&e, "objectid")
                        .map_err(parse_err)?
                        // allow-swallow: a non-numeric id becomes the named error on the next line
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| parse_err("<item> needs a numeric objectid".into()))?;
                    let transform = match attr(&e, "transform").map_err(parse_err)? {
                        None => IDENTITY,
                        Some(t) => {
                            let v: Vec<f64> = t
                                .split_whitespace()
                                .map(|x| x.parse::<f64>())
                                .collect::<Result<_, _>>()
                                .map_err(|_| {
                                    parse_err(format!("transform \"{t}\" is not 12 numbers"))
                                })?;
                            v.try_into().map_err(|_| {
                                parse_err(format!("transform \"{t}\" is not 12 numbers"))
                            })?
                        }
                    };
                    items.push((id, transform, line));
                }
                _ => {}
            }
        }
    }

    if gradient_triangles > 0 {
        warnings.push(format!(
            "{gradient_triangles} triangles have a different colour at each corner; MeshFixer \
             keeps one colour per triangle (the first corner's)"
        ));
    }
    if !ignored_elements.is_empty() {
        warnings.push(format!(
            "ignored 3MF extension elements: {}",
            ignored_elements.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    let mut skipped_properties = 0usize;
    let mut to_model_object = |o: &Obj, transform: [f64; 12]| -> Result<ModelObject, IoError> {
        let mesh = Mesh::new_with(o.positions.clone(), o.triangles.clone(), ctx)?.map_err(|e| {
            IoError::Parse {
                line: o.line,
                message: format!("object {}: {e}", o.id),
            }
        })?;
        // Materials: a palette of the properties the object uses, in
        // first-use order (its default first).
        let mut palette: Vec<Material> = Vec::new();
        let mut resolve = |p: (u32, u32)| -> Result<Option<u32>, IoError> {
            if unsupported_groups.contains(&p.0) {
                skipped_properties += 1;
                return Ok(None);
            }
            let m = groups
                .get(&p.0)
                .ok_or_else(|| IoError::Parse {
                    line: o.line,
                    message: format!(
                        "object {} uses property group {}, which doesn't exist",
                        o.id, p.0
                    ),
                })?
                .get(p.1 as usize)
                .ok_or_else(|| IoError::Parse {
                    line: o.line,
                    message: format!(
                        "object {} uses entry {} of property group {}, which has fewer",
                        o.id, p.1, p.0
                    ),
                })?;
            Ok(Some(match palette.iter().position(|q| q == m) {
                Some(i) => i as u32,
                None => {
                    palette.push(m.clone());
                    (palette.len() - 1) as u32
                }
            }))
        };
        let default = o.property.map(&mut resolve).transpose()?.flatten();
        let mut faces = Vec::with_capacity(o.triangle_properties.len());
        let mut own = false;
        for p in &o.triangle_properties {
            let m = p.map(&mut resolve).transpose()?.flatten();
            own |= m.is_some() && m != default;
            faces.push(m.unwrap_or(NO_MATERIAL));
        }
        let mesh = if palette.is_empty() {
            mesh
        } else {
            mesh.with_materials(palette, default, own.then_some(faces))
        };
        Ok(ModelObject {
            metadata: o.metadata.clone(),
            name: o.name.clone().unwrap_or_else(|| format!("object {}", o.id)),
            mesh,
            transform,
        })
    };
    let mut out = Vec::new();
    if !saw_build || items.is_empty() {
        warnings.push("3MF model has no build items; importing every mesh object".into());
        for o in &objects {
            if o.has_components {
                warnings.push(format!(
                    "object {} uses components, which MeshFixer doesn't support yet; skipped",
                    o.id
                ));
            } else if o.has_mesh {
                out.push(to_model_object(o, IDENTITY)?);
            }
        }
    } else {
        for (id, transform, line) in &items {
            let o = objects.iter().find(|o| o.id == *id).ok_or(IoError::Parse {
                line: *line,
                message: format!("build item references unknown object {id}"),
            })?;
            if o.has_components {
                warnings.push(format!(
                    "object {} uses components, which MeshFixer doesn't support yet; skipped",
                    o.id
                ));
            } else {
                out.push(to_model_object(o, *transform)?);
            }
        }
    }
    if skipped_properties > 0 || !unsupported_groups.is_empty() {
        warnings.push(format!(
            "3MF textures and composite materials aren't imported ({skipped_properties} uses); \
             those faces keep their object's colour, if any"
        ));
    }
    ctx.progress.report(READING, 1.0);
    Ok((Model { unit, objects: out }, digits, warnings))
}

pub fn read(bytes: &[u8]) -> Result<(Model, Vec<String>), IoError> {
    read_with_precision(bytes).map(|(m, _, w)| (m, w))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Encode a model as a 3MF archive. Only live vertices and faces are
/// written. Empty objects (no vertices or triangles — the core schema needs
/// at least 3 vertices and 1 triangle) are left out, since writing one
/// produces an invalid 3MF object (MF-ISSUE-034); use
/// [`write_filtering_empty`] to learn which objects, if any, were left out.
pub fn write(model: &Model) -> Result<Vec<u8>, String> {
    write_filtering_empty(model).map(|(bytes, _warnings)| bytes)
}

/// As [`write`], also returning a warning naming each object left out for
/// being empty (MF-ISSUE-034).
pub fn write_filtering_empty(model: &Model) -> Result<(Vec<u8>, Vec<String>), String> {
    use std::fmt::Write as _;
    let mut warnings = Vec::new();
    let kept: Vec<&ModelObject> = model
        .objects
        .iter()
        .filter(|o| {
            if o.mesh.is_empty() {
                warnings.push(format!(
                    "object '{}' has no geometry; left out of the export",
                    o.name
                ));
                false
            } else {
                true
            }
        })
        .collect();
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\"",
    );
    if kept
        .iter()
        .any(|o| o.metadata.keys().any(|k| k.starts_with("meshfixer:")))
    {
        // allow-swallow: writing to a String can't fail
        let _ = write!(xml, " xmlns:meshfixer=\"{MESHFIXER_NS}\"");
    }
    // allow-swallow: writing to a String can't fail
    let _ = write!(xml, " unit=\"{}\">\n <resources>\n", model.unit.as_attr());
    for (k, o) in kept.iter().enumerate() {
        // allow-swallow: writing to a String can't fail
        // Materials (MF-ISSUE-091): the object's palette as one
        // <basematerials> group (id after every object's), its default as
        // the object's pid/pindex, and each face that differs as pid/p1.
        let group = kept.len() + k + 1;
        let materials = o.mesh.materials();
        let mut object_default = o.mesh.default_material();
        if !materials.is_empty() {
            // allow-swallow: writing to a String can't fail
            let _ = writeln!(xml, "  <basematerials id=\"{group}\">");
            for m in materials {
                let name = if m.name.is_empty() {
                    m.hex()
                } else {
                    m.name.clone()
                };
                // allow-swallow: writing to a String can't fail
                let _ = writeln!(
                    xml,
                    "   <base name=\"{}\" displaycolor=\"{}\"/>",
                    escape(&name),
                    m.hex()
                );
            }
            xml.push_str("  </basematerials>\n");
            if object_default.is_none() {
                // 3MF wants an object pid when triangles have one: use the
                // most common face material.
                let mut count = vec![0usize; materials.len()];
                for (f, _) in o.mesh.faces() {
                    if let Some(m) = o.mesh.face_material(f) {
                        count[m as usize] += 1;
                    }
                }
                object_default = (0..count.len())
                    .max_by_key(|&i| (count[i], std::cmp::Reverse(i)))
                    .map(|i| i as u32);
            }
        }
        let properties = match object_default {
            Some(d) if !materials.is_empty() => format!(" pid=\"{group}\" pindex=\"{d}\""),
            _ => String::new(),
        };
        // allow-swallow: writing to a String can't fail
        let _ = writeln!(
            xml,
            "  <object id=\"{}\" type=\"model\" name=\"{}\"{properties}>",
            k + 1,
            escape(&o.name)
        );
        if !o.metadata.is_empty() {
            xml.push_str("   <metadatagroup>\n");
            for (name, value) in &o.metadata {
                // allow-swallow: writing to a String can't fail
                let _ = writeln!(
                    xml,
                    "    <metadata name=\"{}\">{}</metadata>",
                    escape(name),
                    escape(value)
                );
            }
            xml.push_str("   </metadatagroup>\n");
        }
        xml.push_str("   <mesh>\n    <vertices>\n");
        let mut index = vec![0u32; o.mesh.vertex_slots() as usize];
        for (n, (id, p)) in o.mesh.vertices().enumerate() {
            index[id.0 as usize] = n as u32;
            // allow-swallow: writing to a String can't fail
            let _ = writeln!(
                xml,
                "     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>",
                p[0], p[1], p[2]
            );
        }
        xml.push_str("    </vertices>\n    <triangles>\n");
        for (f, t) in o.mesh.faces() {
            let [a, b, c] = t.map(|v| index[v as usize]);
            let own = o.mesh.face_material(f);
            let p = match own {
                Some(m) if !materials.is_empty() && Some(m) != object_default => {
                    format!(" pid=\"{group}\" p1=\"{m}\"")
                }
                _ => String::new(),
            };
            // allow-swallow: writing to a String can't fail
            let _ = writeln!(xml, "     <triangle v1=\"{a}\" v2=\"{b}\" v3=\"{c}\"{p}/>");
        }
        xml.push_str("    </triangles>\n   </mesh>\n  </object>\n");
    }
    xml.push_str(" </resources>\n <build>\n");
    for (k, o) in kept.iter().enumerate() {
        if o.transform == IDENTITY {
            // allow-swallow: writing to a String can't fail
            let _ = writeln!(xml, "  <item objectid=\"{}\"/>", k + 1);
        } else {
            let t: Vec<String> = o.transform.iter().map(|x| x.to_string()).collect();
            // allow-swallow: writing to a String can't fail
            let _ = writeln!(
                xml,
                "  <item objectid=\"{}\" transform=\"{}\"/>",
                k + 1,
                t.join(" ")
            );
        }
    }
    xml.push_str(" </build>\n</model>\n");

    let mut buf = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);
    let opt = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let entries = [
        (
            "[Content_Types].xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/></Types>\n".to_owned(),
        ),
        (
            "_rels/.rels",
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Target=\"/{DEFAULT_MODEL_PATH}\" Id=\"rel0\" Type=\"{MODEL_REL}\"/></Relationships>\n"),
        ),
        (DEFAULT_MODEL_PATH, xml),
    ];
    for (name, content) in entries {
        zip.start_file(name, opt).map_err(|e| e.to_string())?;
        zip.write_all(content.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok((buf.into_inner(), warnings))
}

/// Encode `model` and write it to `path` in one step (tests and simple
/// callers); the caller-facing export path is [`crate::write_model_path`],
/// which also verifies by reading the bytes back.
pub fn write_checked(model: &Model, path: &std::path::Path) -> Result<Vec<String>, String> {
    let (bytes, warnings) = write_filtering_empty(model)?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(warnings)
}
