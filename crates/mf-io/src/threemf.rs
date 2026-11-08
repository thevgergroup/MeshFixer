//! 3MF core (MF-IO-001, MF-DOC-004): mesh objects, build-item transforms and
//! the model unit. Unsupported features (components, extensions) produce
//! warnings rather than silent loss.

use std::collections::BTreeSet;
use std::io::{Cursor, Read, Write};

use mf_geometry::Mesh;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use serde::{Deserialize, Serialize};

use crate::{IoError, significant_digits};

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
}

#[derive(Debug, Clone)]
pub struct Model {
    pub unit: Unit,
    pub objects: Vec<ModelObject>,
}

const CORE_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
const MODEL_REL: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";
const DEFAULT_MODEL_PATH: &str = "3D/3dmodel.model";

fn not_3mf(message: String) -> IoError {
    IoError::Parse { line: 0, message }
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
        if uri == CORE_NS {
            core.insert(prefix);
        }
    }
    if !default_declared {
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
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| not_3mf(format!("not a 3MF (zip) archive: {e}")))?;
    let mut warnings = Vec::new();
    let read_entry =
        |zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str| -> Result<String, IoError> {
            let mut entry = zip
                .by_name(name)
                .map_err(|e| not_3mf(format!("3MF archive has no '{name}': {e}")))?;
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|e| not_3mf(format!("'{name}' is not UTF-8 text: {e}")))?;
            Ok(text)
        };

    // Locate the model part through the package relationships.
    let model_path = match read_entry(&mut zip, "_rels/.rels") {
        Ok(rels) => {
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
        }
        Err(_) => {
            warnings.push(format!(
                "3MF archive has no _rels/.rels; using the standard path {DEFAULT_MODEL_PATH}"
            ));
            DEFAULT_MODEL_PATH.to_owned()
        }
    };
    let text = read_entry(&mut zip, &model_path)?;

    #[derive(Default)]
    struct Obj {
        id: u32,
        name: Option<String>,
        positions: Vec<[f64; 3]>,
        triangles: Vec<[u32; 3]>,
        has_mesh: bool,
        has_components: bool,
        line: usize,
    }
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
    let mut has_properties = false;
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut lines = LineCounter::new(&text);
    loop {
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
            Event::End(end) => {
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
            }
            let prefix = e.name().prefix().map(|p| p.as_ref().to_owned());
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
                    has_properties |= attr(&e, "pid").map_err(parse_err)?.is_some();
                    let obj = Obj {
                        id,
                        name: attr(&e, "name").map_err(parse_err)?,
                        line,
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
                    has_properties |= attr(&e, "pid").map_err(parse_err)?.is_some()
                        || attr(&e, "p1").map_err(parse_err)?.is_some();
                    current
                        .as_mut()
                        .ok_or_else(|| parse_err("<triangle> outside an object".into()))?
                        .triangles
                        .push(t);
                }
                "components" => {
                    if let Some(o) = current.as_mut() {
                        o.has_components = true;
                    }
                }
                "basematerials" => has_properties = true,
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

    if has_properties {
        warnings.push(
            "3MF material and colour properties are ignored; MeshFixer imports geometry only"
                .into(),
        );
    }
    if !ignored_elements.is_empty() {
        warnings.push(format!(
            "ignored 3MF extension elements: {}",
            ignored_elements.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    let to_model_object = |o: &Obj, transform: [f64; 12]| -> Result<ModelObject, IoError> {
        let mesh =
            Mesh::new(o.positions.clone(), o.triangles.clone()).map_err(|e| IoError::Parse {
                line: o.line,
                message: format!("object {}: {e}", o.id),
            })?;
        Ok(ModelObject {
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

/// Encode a model as a 3MF archive. Only live vertices and faces are written.
pub fn write(model: &Model) -> Result<Vec<u8>, String> {
    use std::fmt::Write as _;
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\"",
    );
    // allow-swallow: writing to a String can't fail
    let _ = write!(xml, " unit=\"{}\">\n <resources>\n", model.unit.as_attr());
    for (k, o) in model.objects.iter().enumerate() {
        // allow-swallow: writing to a String can't fail
        let _ = write!(
            xml,
            "  <object id=\"{}\" type=\"model\" name=\"{}\">\n   <mesh>\n    <vertices>\n",
            k + 1,
            escape(&o.name)
        );
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
        for (_, t) in o.mesh.faces() {
            let [a, b, c] = t.map(|v| index[v as usize]);
            // allow-swallow: writing to a String can't fail
            let _ = writeln!(xml, "     <triangle v1=\"{a}\" v2=\"{b}\" v3=\"{c}\"/>");
        }
        xml.push_str("    </triangles>\n   </mesh>\n  </object>\n");
    }
    xml.push_str(" </resources>\n <build>\n");
    for (k, o) in model.objects.iter().enumerate() {
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
    Ok(buf.into_inner())
}
