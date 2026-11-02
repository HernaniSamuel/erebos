//! Reading an SVD file into the model (ADR 0070, 0072): the XML into a
//! tree of the elements as written, `derivedFrom` resolved, properties
//! inherited, `dim` read.
//!
//! The phases run in order, each only when the one before reported no
//! error, so that one mistake does not bring a string of consequences:
//! parsing the elements, resolving `derivedFrom`, building the model.

use std::collections::HashSet;

use crate::diag::{Diagnostic, Diagnostics};
use crate::span::{FileId, Span};

use super::parse::{self, Parser};
use super::raw::Tree;
use super::{Device, build, derive};

/// The diagnostics of one reading, and of the writing that follows. An element
/// copied for a derived one is built and written where it stands, so the same
/// mistake can be found more than once; it is reported once.
pub(super) struct Reader<'a> {
    diags: &'a mut Diagnostics,
    errors: usize,
    seen: HashSet<(&'static str, Option<Span>, String)>,
}

impl<'a> Reader<'a> {
    pub(super) fn new(diags: &'a mut Diagnostics) -> Self {
        Reader {
            diags,
            errors: 0,
            seen: HashSet::new(),
        }
    }

    pub fn error(&mut self, diag: Diagnostic) {
        if self.first(&diag) {
            self.errors += 1;
            self.diags.push(diag);
        }
    }

    pub fn warning(&mut self, diag: Diagnostic) {
        if self.first(&diag) {
            self.diags.push(diag);
        }
    }

    fn first(&mut self, diag: &Diagnostic) -> bool {
        let at = diag.labels.first().map(|label| label.span);
        self.seen.insert((diag.code, at, diag.message.clone()))
    }

    pub(super) fn failed(&self) -> bool {
        self.errors > 0
    }
}

/// The text of a file, or `svd::xml` without a span when the bytes are not
/// UTF-8: the source map holds text, so a file that is not has no spans.
pub fn utf8<'a>(bytes: &'a [u8], diags: &mut Diagnostics) -> Option<&'a str> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(error) => {
            diags.push(
                Diagnostic::error(
                    "svd::xml",
                    format!(
                        "the file is not UTF-8: byte {} is not valid",
                        error.valid_up_to()
                    ),
                )
                .help("SVD files are read as UTF-8"),
            );
            None
        }
    }
}

/// Reads the SVD file `file`, whose text is `text`, into a [`Device`], or
/// `None` when it reported an error. Warnings alone leave the device.
pub fn read(file: FileId, text: &str, diags: &mut Diagnostics) -> Option<Device> {
    let mut rd = Reader::new(diags);
    let doc = match roxmltree::Document::parse(text) {
        Ok(doc) => doc,
        Err(error) => {
            let at = offset_of(text, error.pos());
            let end = text[at..].chars().next().map_or(at, |c| at + c.len_utf8());
            rd.error(
                Diagnostic::error("svd::xml", error.to_string())
                    .label(Span::new(file, at, end), "here"),
            );
            return None;
        }
    };
    let root = doc.root_element();
    let root_span = Span::new(file, root.range().start, root.range().end);
    if root.tag_name().name() != "device" {
        rd.error(
            Diagnostic::error(
                "svd::not-svd",
                format!(
                    "the root element is `{}`, and an SVD file's is `device`",
                    root.tag_name().name()
                ),
            )
            .label(root_span, "not a `device`"),
        );
        return None;
    }
    let Some(peripherals) = parse::child(root, "peripherals") else {
        rd.error(
            Diagnostic::error("svd::not-svd", "the `device` has no `peripherals`")
                .label(root_span, "no `peripherals` in it"),
        );
        return None;
    };

    let mut parser = Parser {
        file,
        tree: Tree::default(),
        rd: &mut rd,
    };
    let name = match parser.string(root, "name") {
        Some(name) => Some(name.trim().to_owned()),
        None => {
            parser.missing(root, "device", "name");
            None
        }
    };
    // A value that is no SVD value on the device has no item to comment.
    let device_props = parser.props(root, &mut Vec::new());
    parser.peripherals(peripherals);
    let mut tree = parser.tree;
    if rd.failed() {
        return None;
    }
    derive::resolve(&mut tree, &mut rd);
    if rd.failed() {
        return None;
    }
    let peripherals = build::build(&tree, device_props, &mut rd);
    if rd.failed() {
        return None;
    }
    Some(Device {
        span: root_span,
        name: name?,
        version: parse::child(root, "version").map(|v| parse::text_of(v).trim().to_owned()),
        license_text: parse::child(root, "licenseText").map(parse::text_of),
        peripherals,
    })
}

/// The byte offset of a position of roxmltree: the row (from 1) and the
/// column (from 1, in characters).
fn offset_of(text: &str, pos: roxmltree::TextPos) -> usize {
    let row = pos.row.saturating_sub(1) as usize;
    let col = pos.col.saturating_sub(1) as usize;
    let mut start = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i == row {
            let in_line: usize = line.chars().take(col).map(char::len_utf8).sum();
            return start + in_line;
        }
        start += line.len();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Severity;
    use crate::span::SourceMap;
    use crate::svd::{
        Access, Bits, Cluster, EnumNumber, Item, Peripheral, ReadAction, Register, Repeat, Usage,
        WriteEffect,
    };

    // ---- SVD text written in the tests

    /// A device with `props` (elements of the device) and `peripherals`.
    fn svd(props: &str, peripherals: &str) -> String {
        format!(
            "<device>\n<name>DEV</name><version>1.0</version>{props}\n\
             <peripherals>{peripherals}</peripherals>\n</device>"
        )
    }

    /// A peripheral at 0x1000 with `body` in it.
    fn p(name: &str, body: &str) -> String {
        format!(
            "<peripheral><name>{name}</name><baseAddress>0x1000</baseAddress>{body}</peripheral>"
        )
    }

    fn regs(body: &str) -> String {
        format!("<registers>{body}</registers>")
    }

    fn reg_xml(name: &str, offset: &str, size: Option<u32>, body: &str) -> String {
        let size = size.map_or(String::new(), |s| format!("<size>{s}</size>"));
        format!(
            "<register><name>{name}</name><addressOffset>{offset}</addressOffset>{size}{body}\
             </register>"
        )
    }

    /// A register of 32 bits at 0 with `body` in it.
    fn r(name: &str, body: &str) -> String {
        reg_xml(name, "0", Some(32), body)
    }

    /// A register at 0 with no size.
    fn rb(name: &str, body: &str) -> String {
        reg_xml(name, "0", None, body)
    }

    fn rs(name: &str, size: u32, body: &str) -> String {
        reg_xml(name, "0", Some(size), body)
    }

    fn ra(name: &str, offset: &str, body: &str) -> String {
        reg_xml(name, offset, Some(32), body)
    }

    fn fields(body: &str) -> String {
        format!("<fields>{body}</fields>")
    }

    /// A field of one bit at 0 with `body` in it.
    fn f(name: &str, body: &str) -> String {
        format!(
            "<field><name>{name}</name><bitOffset>0</bitOffset><bitWidth>1</bitWidth>{body}\
             </field>"
        )
    }

    /// A set of enumerated values with `body` in it.
    fn set(attrs: &str, body: &str) -> String {
        format!("<enumeratedValues{attrs}>{body}</enumeratedValues>")
    }

    fn value(name: &str, value: &str) -> String {
        format!("<enumeratedValue><name>{name}</name><value>{value}</value></enumeratedValue>")
    }

    /// A peripheral that is the whole of `P`: a register `R` with `body`.
    fn one_register(body: &str) -> String {
        svd("", &p("P", &regs(&r("R", body))))
    }

    /// A peripheral `P` with a register `R` with a field `F` with `body`.
    fn one_field(body: &str) -> String {
        one_register(&fields(&f("F", body)))
    }

    // ---- reading them

    fn read_str(text: &str) -> (Option<Device>, Vec<Diagnostic>) {
        let mut map = SourceMap::default();
        let file = map.add("test.svd", text);
        let mut diags = Diagnostics::default();
        let device = read(file, text, &mut diags);
        (device, diags.into_vec())
    }

    /// The device of a file that reads, and its diagnostics (warnings).
    fn ok(text: &str) -> (Device, Vec<Diagnostic>) {
        let (device, diags) = read_str(text);
        let Some(device) = device else {
            panic!("the file did not read: {diags:#?}");
        };
        (device, diags)
    }

    fn codes(diags: &[Diagnostic]) -> Vec<&'static str> {
        diags.iter().map(|d| d.code).collect()
    }

    /// The one error a file reports: the text of its primary label.
    fn the_error(text: &str, code: &str) -> String {
        let (device, diags) = read_str(text);
        assert!(device.is_none(), "an error leaves no device");
        assert_eq!(codes(&diags), [code], "{diags:#?}");
        text[diags[0].labels[0].span.range()].to_owned()
    }

    fn periph<'a>(device: &'a Device, name: &str) -> &'a Peripheral {
        device
            .peripherals
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no peripheral {name}"))
    }

    fn reg<'a>(items: &'a [Item], name: &str) -> &'a Register {
        items
            .iter()
            .find_map(|item| match item {
                Item::Register(r) if r.name == name => Some(r),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no register {name}"))
    }

    fn cluster<'a>(items: &'a [Item], name: &str) -> &'a Cluster {
        items
            .iter()
            .find_map(|item| match item {
                Item::Cluster(c) if c.name == name => Some(c),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no cluster {name}"))
    }

    fn names(items: &[Item]) -> Vec<&str> {
        items
            .iter()
            .map(|item| match item {
                Item::Register(r) => r.name.as_str(),
                Item::Cluster(c) => c.name.as_str(),
            })
            .collect()
    }

    // ---- the file as a whole

    #[test]
    fn a_device_is_read_with_its_header_and_spans() {
        let text = "<device><name> DEV </name><version>1.2</version>\
                    <licenseText>  Apache \n  2.0  </licenseText>\
                    <peripherals><peripheral><name>A</name><baseAddress>0x40000000</baseAddress>\
                    </peripheral></peripherals></device>";
        let (device, diags) = ok(text);
        assert!(diags.is_empty());
        assert_eq!(device.name, "DEV");
        assert_eq!(device.version.as_deref(), Some("1.2"));
        assert_eq!(device.license_text.as_deref(), Some("  Apache \n  2.0  "));
        let a = periph(&device, "A");
        assert_eq!(a.base_address, 0x4000_0000);
        assert!(text[a.span.range()].starts_with("<peripheral>"));
        assert!(text[a.span.range()].ends_with("</peripheral>"));
        assert_eq!(&text[device.span.range()], text);
    }

    #[test]
    fn a_file_with_a_namespace_and_entities_is_read() {
        let text = "<?xml version=\"1.0\"?>\n\
            <device xmlns:xs=\"http://www.w3.org/2001/XMLSchema-instance\" \
             xs:noNamespaceSchemaLocation=\"CMSIS-SVD.xsd\">\n\
            <name>D&amp;E</name><peripherals><peripheral><name>A</name>\
            <description>a &lt; b <![CDATA[<c>]]></description>\
            <baseAddress>1</baseAddress></peripheral></peripherals></device>";
        let (device, _) = ok(text);
        assert_eq!(device.name, "D&E");
        assert_eq!(
            periph(&device, "A").description.as_deref(),
            Some("a < b <c>")
        );
    }

    #[test]
    fn a_file_that_is_not_utf8_has_no_span() {
        let mut diags = Diagnostics::default();
        assert_eq!(utf8(b"<device>\xff</device>", &mut diags), None);
        let diags = diags.into_vec();
        assert_eq!(codes(&diags), ["svd::xml"]);
        assert!(diags[0].labels.is_empty());
        assert!(diags[0].message.contains("byte 8"));
        let mut diags = Diagnostics::default();
        assert_eq!(utf8("<é/>".as_bytes(), &mut diags), Some("<é/>"));
        assert!(diags.is_empty());
    }

    #[test]
    fn malformed_xml_is_reported_at_its_position() {
        let text = "<device>\n  <name>é</nme>\n</device>";
        let (device, diags) = read_str(text);
        assert!(device.is_none());
        assert_eq!(codes(&diags), ["svd::xml"]);
        // roxmltree counts the columns in characters; the span is in bytes.
        let at = diags[0].labels[0].span;
        assert!(text[..at.lo as usize].ends_with('é'));
        assert_eq!(&text[at.range()], "<");
        for text in ["", "   ", "<device>"] {
            let (device, diags) = read_str(text);
            assert!(device.is_none());
            assert_eq!(codes(&diags), ["svd::xml"], "{text:?}");
        }
    }

    #[test]
    fn positions_of_roxmltree_are_byte_offsets_in_the_text() {
        let text = "ab\ncé<x";
        assert_eq!(offset_of(text, roxmltree::TextPos::new(1, 1)), 0);
        assert_eq!(offset_of(text, roxmltree::TextPos::new(2, 3)), 6);
        assert_eq!(offset_of(text, roxmltree::TextPos::new(9, 1)), text.len());
    }

    #[test]
    fn a_file_that_is_no_svd_is_refused() {
        let text = "<root><peripherals/></root>";
        assert_eq!(the_error(text, "svd::not-svd"), text);
        let text = "<device><name>D</name></device>";
        assert_eq!(the_error(text, "svd::not-svd"), text);
    }

    #[test]
    fn each_missing_element_is_reported_on_its_element() {
        let text = "<device><peripherals/></device>";
        assert_eq!(the_error(text, "svd::missing-element"), text);

        let element = "<peripheral><baseAddress>1</baseAddress></peripheral>";
        assert_eq!(
            the_error(&svd("", element), "svd::missing-element"),
            element
        );
        let element = "<peripheral><name>A</name></peripheral>";
        assert_eq!(
            the_error(&svd("", element), "svd::missing-element"),
            element
        );

        let register = |body: &str| format!("<register>{body}</register>");
        for element in [
            register("<addressOffset>0</addressOffset><size>8</size>"),
            register("<name>R</name><size>8</size>"),
            register("<name>R</name><addressOffset>0</addressOffset>"),
        ] {
            let text = svd("", &p("A", &regs(&element)));
            assert_eq!(the_error(&text, "svd::missing-element"), element);
        }

        let element = "<cluster><name>C</name></cluster>";
        let text = svd("", &p("A", &regs(element)));
        assert_eq!(the_error(&text, "svd::missing-element"), element);

        let element = "<field><name>F</name></field>";
        let text = one_register(&fields(element));
        assert_eq!(the_error(&text, "svd::missing-element"), element);

        let element = "<enumeratedValue><name>V</name></enumeratedValue>";
        let text = one_field(&set("", element));
        assert_eq!(the_error(&text, "svd::missing-element"), element);
    }

    #[test]
    fn a_size_a_device_supplies_is_not_missing() {
        let text = svd(
            "<size>16</size>",
            &p("A", &regs(&reg_xml("R", "4", None, ""))),
        );
        let (device, _) = ok(&text);
        assert_eq!(reg(&periph(&device, "A").items, "R").size, 16);
    }

    #[test]
    fn every_missing_element_of_a_file_is_reported() {
        let text = svd(
            "",
            &p(
                "A",
                &regs(
                    "<register><name>R</name></register>\
                     <register><name>S</name><size>8</size></register>",
                ),
            ),
        );
        let (device, diags) = read_str(&text);
        assert!(device.is_none());
        // R: no offset and no size; S: no offset.
        assert_eq!(codes(&diags), ["svd::missing-element"; 3]);
    }

    // ---- numbers

    #[test]
    fn an_invalid_number_is_reported_on_its_element() {
        let in_register = |tag: &str| one_register(tag);
        let in_field = |tag: &str| {
            let field = format!("<field><name>F</name>{tag}<bitWidth>1</bitWidth></field>");
            one_register(&fields(&field))
        };
        let cases = [
            ("baseAddress", "0x2x"),
            ("baseAddress", "-1"),
            ("baseAddress", "0x10000000000000000"),
            ("addressOffset", "1_000"),
            ("size", "#2"),
            ("size", "0x100000000"),
            ("resetValue", "ten"),
            ("resetMask", ""),
            ("dim", "2.5"),
            ("dimIncrement", "four"),
            ("bitOffset", "x"),
        ];
        for (element, bad) in cases {
            let tag = format!("<{element}>{bad}</{element}>");
            let text = match element {
                "baseAddress" => svd("", &format!("<peripheral><name>A</name>{tag}</peripheral>")),
                "bitOffset" => in_field(&tag),
                "addressOffset" => svd(
                    "",
                    &p(
                        "P",
                        &regs(&format!("<register><name>R</name>{tag}</register>")),
                    ),
                ),
                "size" => svd("", &p("P", &regs(&rb("R", &tag)))),
                // Without a `dim`, a `dimIncrement` is not read.
                "dimIncrement" => in_register(&format!("<dim>2</dim>{tag}")),
                _ => in_register(&tag),
            };
            let (device, diags) = read_str(&text);
            assert!(device.is_none(), "{element} {bad}");
            // A number that cannot be read is no cascade of missing
            // elements: one error, on the element.
            assert_eq!(codes(&diags), ["svd::invalid-number"], "{element} {bad}");
            assert_eq!(&text[diags[0].labels[0].span.range()], tag);
        }
    }

    #[test]
    fn numbers_are_read_in_every_syntax() {
        let text = svd(
            "",
            &p(
                "A",
                &regs(&format!(
                    "{}{}{}",
                    ra("D", "16", "<resetValue>0X1f</resetValue>"),
                    ra("B", "#101", "<resetValue> 0x10 </resetValue>"),
                    ra("H", "0x20", "")
                )),
            ),
        );
        let (device, _) = ok(&text);
        let items = &periph(&device, "A").items;
        assert_eq!(reg(items, "D").offset, 16);
        assert_eq!(reg(items, "D").reset_value, Some(0x1F));
        assert_eq!(reg(items, "B").offset, 5);
        assert_eq!(reg(items, "B").reset_value, Some(0x10));
        assert_eq!(reg(items, "H").offset, 0x20);
    }

    #[test]
    fn a_parse_error_stops_before_the_errors_that_follow_from_it() {
        // `NOPE` is unknown too, but a number that cannot be read comes
        // first: the later phases do not run on a tree with holes.
        let text = svd(
            "",
            &format!(
                "{}{}",
                p("A", "<size>x</size>"),
                "<peripheral derivedFrom=\"NOPE\"><name>B</name></peripheral>"
            ),
        );
        let (_, diags) = read_str(&text);
        assert_eq!(codes(&diags), ["svd::invalid-number"]);
    }

    // ---- inheritance

    #[test]
    fn properties_are_inherited_down_to_the_register() {
        let cluster_xml = format!(
            "<cluster><name>C</name><addressOffset>8</addressOffset>\
             <access>write-only</access><resetMask>0xFF</resetMask>{}{}</cluster>",
            rb("X", ""),
            rs(
                "Y",
                8,
                "<access>read-write</access><resetValue>7</resetValue><resetMask>1</resetMask>"
            )
        );
        let text = svd(
            "<size>32</size><access>read-only</access><resetValue>0xFF</resetValue>\
             <resetMask>0xF0</resetMask>",
            &format!(
                "{}{}",
                // Nothing of its own: the device's.
                p("A", &regs(&rb("R", ""))),
                // The peripheral states some, a cluster some more, a
                // register its own.
                p(
                    "B",
                    &format!(
                        "<size>16</size><resetValue>3</resetValue>{}",
                        regs(&cluster_xml)
                    )
                ),
            ),
        );
        let (device, _) = ok(&text);
        let a = reg(&periph(&device, "A").items, "R");
        assert_eq!(a.size, 32);
        assert_eq!(a.access, Some(Access::ReadOnly));
        assert_eq!((a.reset_value, a.reset_mask), (Some(0xFF), 0xF0));
        let c = cluster(&periph(&device, "B").items, "C");
        // The size and reset value from the peripheral, the access and the
        // mask from the cluster.
        let x = reg(&c.items, "X");
        assert_eq!(x.size, 16);
        assert_eq!(x.access, Some(Access::WriteOnly));
        assert_eq!((x.reset_value, x.reset_mask), (Some(3), 0xFF));
        let y = reg(&c.items, "Y");
        assert_eq!(y.size, 8);
        assert_eq!(y.access, Some(Access::ReadWrite));
        assert_eq!((y.reset_value, y.reset_mask), (Some(7), 1));
    }

    #[test]
    fn a_register_without_reset_value_or_mask_has_none_and_every_bit() {
        let text = svd(
            "",
            &p(
                "A",
                &regs(&format!(
                    "{}{}{}",
                    r("W", ""),
                    rs("H", 16, ""),
                    rs("L", 64, "")
                )),
            ),
        );
        let (device, _) = ok(&text);
        let items = &periph(&device, "A").items;
        assert_eq!(reg(items, "W").reset_value, None);
        assert_eq!(reg(items, "W").access, None);
        assert_eq!(reg(items, "W").reset_mask, 0xFFFF_FFFF);
        assert_eq!(reg(items, "H").reset_mask, 0xFFFF);
        assert_eq!(reg(items, "L").reset_mask, u64::MAX);
    }

    #[test]
    fn reset_bits_above_the_size_of_the_register_are_not_read() {
        let text = svd(
            "<resetValue>0x1FF</resetValue><resetMask>0xFFFF</resetMask>",
            &p(
                "A",
                &regs(&format!("{}{}", rs("B", 8, ""), rs("C", 64, ""))),
            ),
        );
        let (device, _) = ok(&text);
        let items = &periph(&device, "A").items;
        assert_eq!(reg(items, "B").reset_value, Some(0xFF));
        assert_eq!(reg(items, "B").reset_mask, 0xFF);
        assert_eq!(reg(items, "C").reset_value, Some(0x1FF));
        assert_eq!(reg(items, "C").reset_mask, 0xFFFF);
    }

    // ---- derivedFrom

    #[test]
    fn a_derived_peripheral_without_registers_shares_its_base_type() {
        let text = svd(
            "",
            &format!(
                "{}{}{}",
                p("A", &regs(&r("R", ""))),
                "<peripheral derivedFrom=\"A\"><name>B</name><baseAddress>0x2000</baseAddress>\
                 <description>second</description></peripheral>",
                "<peripheral derivedFrom=\"B\"><name>C</name><baseAddress>0x3000</baseAddress>\
                 </peripheral>"
            ),
        );
        let (device, diags) = ok(&text);
        assert!(diags.is_empty());
        let (a, b, c) = (
            periph(&device, "A"),
            periph(&device, "B"),
            periph(&device, "C"),
        );
        assert_eq!(a.shares_type_with, None);
        assert_eq!(b.shares_type_with, Some(0));
        // The type of the last of the chain that has one of its own.
        assert_eq!(c.shares_type_with, Some(0));
        assert_eq!((b.base_address, c.base_address), (0x2000, 0x3000));
        assert_eq!(b.description.as_deref(), Some("second"));
        assert!(b.items.is_empty() && c.items.is_empty());
        assert_eq!(names(&a.items), ["R"]);
    }

    #[test]
    fn a_derived_peripheral_takes_the_address_and_description_it_does_not_state() {
        let text = svd(
            "",
            &format!(
                "{}{}",
                p(
                    "A",
                    &format!("<description>the base</description>{}", regs(&r("R", "")))
                ),
                "<peripheral derivedFrom=\"A\"><name>B</name></peripheral>"
            ),
        );
        let (device, _) = ok(&text);
        let b = periph(&device, "B");
        assert_eq!(b.base_address, 0x1000);
        assert_eq!(b.description.as_deref(), Some("the base"));
    }

    #[test]
    fn a_derived_peripheral_with_a_property_of_its_own_has_a_type_of_its_own() {
        for own in [
            "<size>16</size>",
            "<access>read-only</access>",
            "<resetValue>5</resetValue>",
            "<resetMask>1</resetMask>",
        ] {
            let text = svd(
                "",
                &format!(
                    "{}<peripheral derivedFrom=\"A\"><name>B</name>{own}</peripheral>",
                    p("A", &format!("<size>8</size>{}", regs(&rb("R", ""))))
                ),
            );
            let (device, _) = ok(&text);
            let b = periph(&device, "B");
            assert_eq!(b.shares_type_with, None, "{own}");
            // The registers are the base's, built under the derived
            // peripheral's own properties.
            assert_eq!(names(&b.items), ["R"], "{own}");
        }
        let text = svd(
            "",
            &format!(
                "{}<peripheral derivedFrom=\"A\"><name>B</name><resetValue>5</resetValue>\
                 </peripheral>",
                p("A", &regs(&r("R", "")))
            ),
        );
        let (device, _) = ok(&text);
        assert_eq!(reg(&periph(&device, "B").items, "R").reset_value, Some(5));
        assert_eq!(reg(&periph(&device, "A").items, "R").reset_value, None);
    }

    #[test]
    fn a_derived_peripheral_with_registers_merges_them_with_its_base() {
        let text = svd(
            "",
            &format!(
                "{}<peripheral derivedFrom=\"A\"><name>B</name>{}</peripheral>",
                p(
                    "A",
                    &regs(&format!(
                        "{}{}{}",
                        r("X", ""),
                        ra("Y", "4", ""),
                        ra("Z", "8", "")
                    ))
                ),
                regs(&format!("{}{}", ra("NEW", "12", ""), ra("Y", "0x40", "")))
            ),
        );
        let (device, _) = ok(&text);
        let b = periph(&device, "B");
        assert_eq!(b.shares_type_with, None);
        // Y replaced where the base had it; NEW added after the others.
        assert_eq!(names(&b.items), ["X", "Y", "Z", "NEW"]);
        assert_eq!(reg(&b.items, "Y").offset, 0x40);
        let a = periph(&device, "A");
        assert_eq!(names(&a.items), ["X", "Y", "Z"]);
        assert_eq!(reg(&a.items, "Y").offset, 4);
    }

    #[test]
    fn a_derived_register_takes_what_its_base_states_and_inherits_from_its_own_parent() {
        let derived = |name: &str, base: &str, offset: &str| {
            format!(
                "<register derivedFrom=\"{base}\"><name>{name}</name>\
                 <addressOffset>{offset}</addressOffset></register>"
            )
        };
        let text = svd(
            "",
            &format!(
                "{}{}",
                p(
                    "A",
                    &format!(
                        "<size>32</size><access>read-only</access>{}",
                        regs(&format!(
                            "{}{}",
                            rb("PLAIN", ""),
                            rs("OWN", 8, "<access>write-only</access>")
                        ))
                    )
                ),
                p(
                    "B",
                    &format!(
                        "<size>16</size><access>read-write</access>{}",
                        regs(&format!(
                            "{}{}",
                            derived("D1", "A.PLAIN", "4"),
                            derived("D2", "A.OWN", "8")
                        ))
                    )
                ),
            ),
        );
        let (device, _) = ok(&text);
        let items = &periph(&device, "B").items;
        // PLAIN states nothing: D1 inherits from B.
        let d1 = reg(items, "D1");
        assert_eq!(
            (d1.size, d1.access, d1.offset),
            (16, Some(Access::ReadWrite), 4)
        );
        // OWN states its size and access: D2 has them.
        let d2 = reg(items, "D2");
        assert_eq!(
            (d2.size, d2.access, d2.offset),
            (8, Some(Access::WriteOnly), 8)
        );
    }

    #[test]
    fn a_register_derives_from_a_sibling_by_name_and_from_another_by_path() {
        let derived = |name: &str, base: &str, offset: &str| {
            format!(
                "<register derivedFrom=\"{base}\"><name>{name}</name>\
                 <addressOffset>{offset}</addressOffset></register>"
            )
        };
        let text = svd(
            "",
            &format!(
                "{}{}",
                p(
                    "A",
                    &regs(&format!(
                        "{}{}<cluster><name>C</name><addressOffset>0x10</addressOffset>{}\
                         </cluster>",
                        r(
                            "BASE",
                            "<description>base</description><readAction>clear</readAction>"
                        ),
                        derived("BYNAME", "BASE", "4"),
                        r("INNER", "<resetValue>9</resetValue>")
                    ))
                ),
                p(
                    "B",
                    &regs(&format!(
                        "{}{}",
                        derived("BYPATH", "A.BASE", "4"),
                        derived("DEEP", "A.C.INNER", "8")
                    ))
                ),
            ),
        );
        let (device, _) = ok(&text);
        let byname = reg(&periph(&device, "A").items, "BYNAME");
        assert_eq!(byname.description.as_deref(), Some("base"));
        assert_eq!(byname.read_action, Some(ReadAction::Clear));
        assert_eq!(byname.offset, 4);
        let b = periph(&device, "B");
        assert_eq!(reg(&b.items, "BYPATH").read_action, Some(ReadAction::Clear));
        assert_eq!(reg(&b.items, "DEEP").reset_value, Some(9));
    }

    #[test]
    fn a_name_without_a_dot_is_looked_up_among_the_siblings_only() {
        // `OTHER` is a register of `A`, not a sibling of `R` of `B`.
        let text = svd(
            "",
            &format!(
                "{}{}",
                p("A", &regs(&r("OTHER", ""))),
                p(
                    "B",
                    &regs("<register derivedFrom=\"OTHER\"><name>R</name></register>")
                ),
            ),
        );
        assert_eq!(the_error(&text, "svd::unknown-reference"), "OTHER");
    }

    #[test]
    fn a_cluster_derives_by_name_and_by_path_and_children_merge() {
        let text = svd(
            "",
            &p(
                "A",
                &regs(&format!(
                    "<cluster><name>C</name><addressOffset>0x20</addressOffset>{}{}</cluster>\
                     <cluster derivedFrom=\"C\"><name>D</name>\
                     <addressOffset>0x40</addressOffset>{}{}</cluster>\
                     <cluster derivedFrom=\"A.C\"><name>E</name></cluster>",
                    r("ONE", ""),
                    ra("TWO", "4", ""),
                    ra("TWO", "0x30", ""),
                    r("THREE", "")
                )),
            ),
        );
        let (device, _) = ok(&text);
        let items = &periph(&device, "A").items;
        let (c, d, e) = (
            cluster(items, "C"),
            cluster(items, "D"),
            cluster(items, "E"),
        );
        assert_eq!(names(&c.items), ["ONE", "TWO"]);
        assert_eq!(reg(&c.items, "TWO").offset, 4);
        assert_eq!(names(&d.items), ["ONE", "TWO", "THREE"]);
        assert_eq!(reg(&d.items, "TWO").offset, 0x30);
        assert_eq!(d.offset, 0x40);
        // `E` states no offset of its own: the base's.
        assert_eq!(e.offset, 0x20);
        assert_eq!(names(&e.items), ["ONE", "TWO"]);
    }

    #[test]
    fn a_field_derives_its_bits_and_enumerated_values() {
        let text = one_register(&fields(&format!(
            "<field><name>F</name><bitRange>[3:2]</bitRange><access>read-only</access>{}</field>\
             <field derivedFrom=\"F\"><name>G</name></field>\
             <field derivedFrom=\"P.R.F\"><name>H</name><lsb>8</lsb><msb>11</msb></field>",
            set("", &(value("A", "0") + &value("B", "1")))
        )));
        let (device, _) = ok(&text);
        let fields = &reg(&periph(&device, "P").items, "R").fields;
        let (f, g, h) = (&fields[0], &fields[1], &fields[2]);
        assert_eq!(
            g.bits,
            Bits {
                offset: 2,
                width: 2
            }
        );
        assert_eq!(g.access, Some(Access::ReadOnly));
        assert_eq!(g.enums.len(), 1);
        assert_eq!(g.enums[0].values.len(), 2);
        // The copy is a set of its own.
        assert_ne!(g.enums[0].id, f.enums[0].id);
        assert_eq!(
            h.bits,
            Bits {
                offset: 8,
                width: 4
            }
        );
        assert_eq!(h.access, Some(Access::ReadOnly));
    }

    #[test]
    fn a_set_of_enumerated_values_derives_by_name_and_by_path() {
        let text = svd(
            "",
            &format!(
                "{}{}",
                p(
                    "P",
                    &regs(&r(
                        "R",
                        &fields(&format!(
                            "{}{}",
                            f(
                                "F1",
                                &set(
                                    "",
                                    &format!(
                                        "<name>S1</name>{}{}",
                                        value("A", "0"),
                                        value("B", "1")
                                    )
                                )
                            ),
                            // by name, among the sets of every field of the register
                            f("F2", &set(" derivedFrom=\"S1\"", &value("C", "2")))
                        )),
                    ))
                ),
                p(
                    "Q",
                    &regs(&r(
                        "R",
                        &fields(&f("F3", &set(" derivedFrom=\"P.R.F1.S1\"", ""))),
                    ))
                ),
            ),
        );
        let (device, _) = ok(&text);
        let p_fields = &reg(&periph(&device, "P").items, "R").fields;
        let s1 = &p_fields[0].enums[0];
        let s2 = &p_fields[1].enums[0];
        assert_eq!(s1.derived_from, None);
        assert_eq!(s2.derived_from, Some(s1.id));
        // The base's values, the derived set's own added after.
        let values: Vec<_> = s2.values.iter().map(|v| v.name.as_deref()).collect();
        assert_eq!(values, [Some("A"), Some("B"), Some("C")]);
        let s3 = &reg(&periph(&device, "Q").items, "R").fields[0].enums[0];
        assert_eq!(s3.derived_from, Some(s1.id));
        assert_eq!(s3.values.len(), 2);
    }

    #[test]
    fn the_values_of_a_derived_set_replace_those_of_the_same_name() {
        let text = one_register(&fields(&format!(
            "{}{}",
            f(
                "F1",
                &set(
                    "",
                    &format!("<name>S</name>{}{}", value("A", "0"), value("B", "1"))
                )
            ),
            f(
                "F2",
                &set(
                    " derivedFrom=\"S\"",
                    &format!(
                        "{}<enumeratedValue><value>2</value></enumeratedValue>",
                        value("A", "3")
                    )
                )
            )
        )));
        let (device, _) = ok(&text);
        let set = &reg(&periph(&device, "P").items, "R").fields[1].enums[0];
        let values: Vec<_> = set
            .values
            .iter()
            .map(|v| (v.name.as_deref(), v.value.clone()))
            .collect();
        assert_eq!(
            values,
            [
                (Some("A"), Some(EnumNumber::Value(3))),
                (Some("B"), Some(EnumNumber::Value(1))),
                (None, Some(EnumNumber::Value(2))),
            ]
        );
    }

    #[test]
    fn an_unknown_derived_from_is_reported_on_its_name() {
        let derived_register = |attr: &str| {
            svd(
                "",
                &p(
                    "A",
                    &regs(&format!(
                        "<register derivedFrom=\"{attr}\"><name>R</name></register>"
                    )),
                ),
            )
        };
        let cases = [
            (
                svd(
                    "",
                    "<peripheral derivedFrom=\"NOPE\"><name>A</name></peripheral>",
                ),
                "NOPE",
            ),
            (derived_register("NOPE"), "NOPE"),
            (derived_register("A.NOPE.R"), "A.NOPE.R"),
            (
                one_register(&fields(
                    "<field derivedFrom=\"P.R.NOPE\"><name>F</name></field>",
                )),
                "P.R.NOPE",
            ),
            (one_field(&set(" derivedFrom=\"NOPE\"", "")), "NOPE"),
            (
                // A name of another kind is no element of this one.
                svd(
                    "",
                    &p(
                        "A",
                        &regs(&format!(
                            "{}<cluster derivedFrom=\"R\"><name>C</name></cluster>",
                            r("R", "")
                        )),
                    ),
                ),
                "R",
            ),
        ];
        for (text, name) in cases {
            assert_eq!(the_error(&text, "svd::unknown-reference"), name, "{text}");
        }
    }

    #[test]
    fn an_unknown_alternate_peripheral_is_reported_on_its_element() {
        let text = svd(
            "",
            &format!(
                "{}{}",
                p("A", ""),
                p("B", "<alternatePeripheral>NOPE</alternatePeripheral>")
            ),
        );
        assert_eq!(
            the_error(&text, "svd::unknown-reference"),
            "<alternatePeripheral>NOPE</alternatePeripheral>"
        );
        let text = svd(
            "",
            &format!(
                "{}{}",
                p("A", ""),
                p("B", "<alternatePeripheral>A</alternatePeripheral>")
            ),
        );
        let (device, _) = ok(&text);
        assert_eq!(periph(&device, "B").alternate, Some(0));
        assert_eq!(periph(&device, "A").alternate, None);
    }

    #[test]
    fn elements_deriving_from_each_other_are_a_cycle() {
        let text = svd(
            "",
            &format!(
                "{}{}{}",
                "<peripheral derivedFrom=\"B\"><name>A</name></peripheral>",
                "<peripheral derivedFrom=\"A\"><name>B</name></peripheral>",
                p("C", ""),
            ),
        );
        let (device, diags) = read_str(&text);
        assert!(device.is_none());
        assert_eq!(codes(&diags), ["svd::derived-cycle"]);
        assert!(diags[0].labels[0].message.contains("->"));
        // An element deriving from itself.
        let text = svd(
            "",
            &p(
                "A",
                &regs("<register derivedFrom=\"R\"><name>R</name></register>"),
            ),
        );
        assert_eq!(the_error(&text, "svd::derived-cycle"), "R");
        // Through paths, two registers of two peripherals.
        let derived = |base: &str| {
            p(
                if base == "B.R" { "A" } else { "B" },
                &regs(&format!(
                    "<register derivedFrom=\"{base}\"><name>R</name></register>"
                )),
            )
        };
        let text = svd("", &format!("{}{}", derived("B.R"), derived("A.R")));
        let (_, diags) = read_str(&text);
        assert_eq!(codes(&diags), ["svd::derived-cycle"]);
    }

    #[test]
    fn a_copy_that_has_an_error_reports_it_once() {
        // `R` has no size anywhere; `B` is a copy of `A` with registers.
        let text = svd(
            "",
            &format!(
                "{}<peripheral derivedFrom=\"A\"><name>B</name>{}</peripheral>",
                p("A", &regs(&reg_xml("R", "0", None, ""))),
                regs(&r("EXTRA", "")),
            ),
        );
        let (device, diags) = read_str(&text);
        assert!(device.is_none());
        assert_eq!(codes(&diags), ["svd::missing-element"]);
    }

    // ---- bits

    #[test]
    fn a_field_takes_the_first_form_of_bits_it_states() {
        let bits_of = |body: &str| {
            let text = one_register(&fields(&format!("<field><name>F</name>{body}</field>")));
            let (device, _) = ok(&text);
            reg(&periph(&device, "P").items, "R").fields[0].bits
        };
        let offset_width = "<bitOffset>4</bitOffset><bitWidth>2</bitWidth>";
        let lsb_msb = "<lsb>8</lsb><msb>15</msb>";
        let range = "<bitRange>[23:16]</bitRange>";
        assert_eq!(
            bits_of(offset_width),
            Bits {
                offset: 4,
                width: 2
            }
        );
        assert_eq!(
            bits_of(lsb_msb),
            Bits {
                offset: 8,
                width: 8
            }
        );
        assert_eq!(
            bits_of(range),
            Bits {
                offset: 16,
                width: 8
            }
        );
        assert_eq!(
            bits_of(&format!("{range}{lsb_msb}{offset_width}")),
            Bits {
                offset: 4,
                width: 2
            }
        );
        assert_eq!(
            bits_of(&format!("{range}{lsb_msb}")),
            Bits {
                offset: 8,
                width: 8
            }
        );
        // A form that is not whole is not stated.
        assert_eq!(
            bits_of(&format!("<bitOffset>3</bitOffset>{range}")),
            Bits {
                offset: 16,
                width: 8
            }
        );
    }

    #[test]
    fn bits_that_cannot_be_read_are_an_error() {
        for body in [
            "<bitRange>7:0</bitRange>",
            "<bitRange>[0:7]</bitRange>",
            "<lsb>5</lsb><msb>4</msb>",
            "<bitOffset>0</bitOffset><bitWidth>0</bitWidth>",
        ] {
            let text = one_register(&fields(&format!("<field><name>F</name>{body}</field>")));
            let (device, diags) = read_str(&text);
            assert!(device.is_none(), "{body}");
            assert_eq!(codes(&diags), ["svd::invalid-number"], "{body}");
        }
    }

    // ---- dim

    fn dim_of(name: &str, dim: &str) -> Option<Repeat> {
        let text = svd("", &p("P", &regs(&r(name, dim))));
        let (device, _) = ok(&text);
        reg(&periph(&device, "P").items, name).repeat.clone()
    }

    fn list(indices: &[&str], step: u32) -> Option<Repeat> {
        Some(Repeat::List {
            indices: indices.iter().map(|s| (*s).to_owned()).collect(),
            step,
        })
    }

    #[test]
    fn dim_with_an_array_name_is_an_array() {
        let dim = "<dim>4</dim><dimIncrement>8</dimIncrement>";
        assert_eq!(dim_of("R", ""), None);
        assert_eq!(
            dim_of("R[%s]", dim),
            Some(Repeat::Array { count: 4, step: 8 })
        );
        // The indices are `0` to `dim - 1`, so it is still an array.
        assert_eq!(
            dim_of("R[%s]", &format!("{dim}<dimIndex>0-3</dimIndex>")),
            Some(Repeat::Array { count: 4, step: 8 })
        );
        assert_eq!(
            dim_of("R[%s]", &format!("{dim}<dimIndex>0,1,2,3</dimIndex>")),
            Some(Repeat::Array { count: 4, step: 8 })
        );
    }

    #[test]
    fn dim_makes_a_list_in_the_other_cases() {
        let dim = |count: u32, index: &str| {
            let index = if index.is_empty() {
                String::new()
            } else {
                format!("<dimIndex>{index}</dimIndex>")
            };
            format!("<dim>{count}</dim><dimIncrement>4</dimIncrement>{index}")
        };
        // A `%s` name without brackets: one register per index.
        assert_eq!(dim_of("CH%s_C", &dim(3, "")), list(&["0", "1", "2"], 4));
        assert_eq!(dim_of("CH%s_C", &dim(3, "A-C")), list(&["A", "B", "C"], 4));
        assert_eq!(
            dim_of("CH%s_C", &dim(3, "X, Y,Z")),
            list(&["X", "Y", "Z"], 4)
        );
        assert_eq!(dim_of("CH%s_C", &dim(2, "2-3")), list(&["2", "3"], 4));
        // An array name whose indices are not `0` to `dim - 1`.
        assert_eq!(dim_of("R[%s]", &dim(2, "2-3")), list(&["2", "3"], 4));
        assert_eq!(dim_of("R[%s]", &dim(2, "A,B")), list(&["A", "B"], 4));
        assert_eq!(dim_of("R[%s]", &dim(2, "1,0")), list(&["1", "0"], 4));
    }

    #[test]
    fn dim_on_a_peripheral_or_a_field_is_always_a_list() {
        let text = svd(
            "",
            &format!(
                "{}{}",
                p(
                    "UART[%s]",
                    "<dim>2</dim><dimIncrement>0x1000</dimIncrement>"
                ),
                p(
                    "T%s",
                    &format!(
                        "<dim>2</dim><dimIncrement>0x100</dimIncrement>{}",
                        regs(&r(
                            "R",
                            &fields(&f("TX%s", "<dim>3</dim><dimIncrement>2</dimIncrement>")),
                        ))
                    ),
                )
            ),
        );
        let (device, _) = ok(&text);
        assert_eq!(
            periph(&device, "UART[%s]").repeat,
            list(&["0", "1"], 0x1000)
        );
        let t = periph(&device, "T%s");
        assert_eq!(t.repeat, list(&["0", "1"], 0x100));
        assert_eq!(
            reg(&t.items, "R").fields[0].repeat,
            list(&["0", "1", "2"], 2)
        );
    }

    #[test]
    fn dim_on_a_cluster_follows_the_same_rule() {
        let text = svd(
            "",
            &p(
                "P",
                &regs(
                    "<cluster><name>CTX[%s]</name><addressOffset>0</addressOffset>\
                     <dim>2</dim><dimIncrement>0x1000</dimIncrement></cluster>\
                     <cluster><name>S%s</name><addressOffset>0</addressOffset>\
                     <dim>2</dim><dimIncrement>0x10</dimIncrement></cluster>",
                ),
            ),
        );
        let (device, _) = ok(&text);
        let items = &periph(&device, "P").items;
        assert_eq!(
            cluster(items, "CTX[%s]").repeat,
            Some(Repeat::Array {
                count: 2,
                step: 0x1000
            })
        );
        assert_eq!(cluster(items, "S%s").repeat, list(&["0", "1"], 0x10));
    }

    #[test]
    fn each_invalid_dim_is_reported_on_its_element() {
        let cases = [
            // dim 0
            ("R[%s]", "<dim>0</dim><dimIncrement>4</dimIncrement>"),
            // no dimIncrement
            ("R[%s]", "<dim>2</dim>"),
            // no `%s`
            ("R", "<dim>2</dim><dimIncrement>4</dimIncrement>"),
            // a dimIndex of the wrong length
            (
                "R%s",
                "<dim>2</dim><dimIncrement>4</dimIncrement><dimIndex>A,B,C</dimIndex>",
            ),
            (
                "R%s",
                "<dim>2</dim><dimIncrement>4</dimIncrement><dimIndex>0-2</dimIndex>",
            ),
            // no list, no range
            (
                "R%s",
                "<dim>2</dim><dimIncrement>4</dimIncrement><dimIndex>A B</dimIndex>",
            ),
            (
                "R%s",
                "<dim>2</dim><dimIncrement>4</dimIncrement><dimIndex>A-</dimIndex>",
            ),
            (
                "R%s",
                "<dim>2</dim><dimIncrement>4</dimIncrement><dimIndex>2-1</dimIndex>",
            ),
            // a range that does not fit in 64 bits when counted
            (
                "R%s",
                "<dim>2</dim><dimIncrement>4</dimIncrement>\
                 <dimIndex>0-18446744073709551615</dimIndex>",
            ),
            // too many to write out
            ("R%s", "<dim>70000</dim><dimIncrement>4</dimIncrement>"),
            (
                "R[%s]",
                "<dim>70000</dim><dimIncrement>4</dimIncrement><dimIndex>1-70000</dimIndex>",
            ),
        ];
        for (name, dim) in cases {
            let register = r(name, dim);
            let text = svd("", &p("P", &regs(&register)));
            assert_eq!(
                the_error(&text, "svd::invalid-dim"),
                register,
                "{name} {dim}"
            );
        }
        // An array of any size is no list: no limit.
        assert_eq!(
            dim_of(
                "R[%s]",
                "<dim>4000000000</dim><dimIncrement>4</dimIncrement>"
            ),
            Some(Repeat::Array {
                count: 4_000_000_000,
                step: 4
            })
        );
    }

    #[test]
    fn dim_of_a_peripheral_and_a_field_is_checked_too() {
        let text = svd("", &p("A", "<dim>2</dim><dimIncrement>4</dimIncrement>"));
        assert!(the_error(&text, "svd::invalid-dim").starts_with("<peripheral>"));
        let text = one_register(&fields(&f(
            "F",
            "<dim>2</dim><dimIncrement>1</dimIncrement>",
        )));
        assert!(the_error(&text, "svd::invalid-dim").starts_with("<field>"));
    }

    // ---- values that are no SVD value

    #[test]
    fn a_value_that_is_no_svd_value_is_read_as_absent_with_a_warning() {
        let text = svd(
            "",
            &p(
                "P",
                &format!(
                    "<access>read-write</access>{}",
                    regs(&r(
                        "R",
                        &format!(
                            "<access>write</access><readAction>zap</readAction>\
                             <modifiedWriteValues>oneToFly</modifiedWriteValues>{}",
                            fields(&f(
                                "F",
                                &format!(
                                    "<access>read/clear</access>{}",
                                    set("", &format!("<usage>both</usage>{}", value("A", "0")))
                                )
                            ))
                        )
                    ))
                ),
            ),
        );
        let (device, diags) = ok(&text);
        let register = reg(&periph(&device, "P").items, "R");
        // The register's own `write` is absent: the peripheral's mode.
        assert_eq!(register.access, Some(Access::ReadWrite));
        assert_eq!(register.read_action, None);
        assert_eq!(register.modified_write_values, None);
        assert_eq!(register.notes.len(), 3);
        assert!(register.notes[0].contains("`write`") && register.notes[0].contains("`access`"));
        assert!(register.notes[1].contains("`readAction`"));
        assert!(register.notes[2].contains("`modifiedWriteValues`"));
        let field = &register.fields[0];
        assert_eq!(field.access, None);
        assert_eq!(field.notes.len(), 1);
        assert!(field.notes[0].contains("`read/clear`"));
        assert_eq!(field.enums[0].usage, None);
        assert_eq!(field.enums[0].notes.len(), 1);
        assert_eq!(codes(&diags), ["svd::invalid-value"; 5]);
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
        assert_eq!(
            &text[diags[0].labels[0].span.range()],
            "<access>write</access>"
        );
    }

    #[test]
    fn a_register_with_such_a_value_has_no_mode_when_no_parent_states_one() {
        let (device, diags) = ok(&one_register("<access>write</access>"));
        assert_eq!(reg(&periph(&device, "P").items, "R").access, None);
        assert_eq!(codes(&diags), ["svd::invalid-value"]);
    }

    #[test]
    fn a_value_that_is_no_svd_value_on_the_device_is_warned_without_a_note() {
        let text = svd("<access>write</access>", &p("A", &regs(&r("R", ""))));
        let (device, diags) = ok(&text);
        assert_eq!(codes(&diags), ["svd::invalid-value"]);
        assert_eq!(reg(&periph(&device, "A").items, "R").access, None);
        assert!(periph(&device, "A").notes.is_empty());
    }

    #[test]
    fn every_value_svd_defines_is_read() {
        let access = [
            "read-only",
            "write-only",
            "read-write",
            "writeOnce",
            "read-writeOnce",
        ];
        let actions = ["clear", "set", "modify", "modifyExternal"];
        let effects = [
            "oneToClear",
            "oneToSet",
            "oneToToggle",
            "zeroToClear",
            "zeroToSet",
            "zeroToToggle",
            "clear",
            "set",
            "modify",
        ];
        let mut registers = String::new();
        for (i, a) in access.iter().enumerate() {
            registers += &r(&format!("A{i}"), &format!("<access>{a}</access>"));
        }
        for (i, a) in actions.iter().enumerate() {
            registers += &r(&format!("R{i}"), &format!("<readAction>{a}</readAction>"));
        }
        for (i, e) in effects.iter().enumerate() {
            registers += &r(
                &format!("W{i}"),
                &format!("<modifiedWriteValues>{e}</modifiedWriteValues>"),
            );
        }
        let (device, diags) = ok(&svd("", &p("P", &regs(&registers))));
        assert!(diags.is_empty());
        let items = &periph(&device, "P").items;
        let read = |prefix: &str, n: usize| -> Vec<&Register> {
            (0..n)
                .map(|i| reg(items, &format!("{prefix}{i}")))
                .collect()
        };
        let access: Vec<_> = read("A", 5).iter().map(|r| r.access).collect();
        assert_eq!(
            access,
            [
                Some(Access::ReadOnly),
                Some(Access::WriteOnly),
                Some(Access::ReadWrite),
                Some(Access::WriteOnce),
                Some(Access::ReadWriteOnce)
            ]
        );
        let actions: Vec<_> = read("R", 4).iter().map(|r| r.read_action).collect();
        assert_eq!(
            actions,
            [
                Some(ReadAction::Clear),
                Some(ReadAction::Set),
                Some(ReadAction::Modify),
                Some(ReadAction::ModifyExternal)
            ]
        );
        let effects: Vec<_> = read("W", 9)
            .iter()
            .map(|r| r.modified_write_values)
            .collect();
        assert_eq!(
            effects,
            [
                Some(WriteEffect::OneToClear),
                Some(WriteEffect::OneToSet),
                Some(WriteEffect::OneToToggle),
                Some(WriteEffect::ZeroToClear),
                Some(WriteEffect::ZeroToSet),
                Some(WriteEffect::ZeroToToggle),
                Some(WriteEffect::Clear),
                Some(WriteEffect::Set),
                Some(WriteEffect::Modify)
            ]
        );
    }

    #[test]
    fn the_usage_of_a_set_is_read() {
        let sets: String = ["read", "write", "read-write"]
            .iter()
            .map(|usage| set("", &format!("<usage>{usage}</usage>{}", value("A", "0"))))
            .collect();
        let (device, diags) = ok(&one_field(&sets));
        assert!(diags.is_empty());
        let field = &reg(&periph(&device, "P").items, "R").fields[0];
        let usages: Vec<_> = field.enums.iter().map(|s| s.usage).collect();
        assert_eq!(
            usages,
            [
                Some(Usage::Read),
                Some(Usage::Write),
                Some(Usage::ReadWrite)
            ]
        );
    }

    #[test]
    fn is_default_reads_an_xml_boolean() {
        let values = "<enumeratedValue><name>A</name><isDefault>1</isDefault></enumeratedValue>\
                      <enumeratedValue><name>B</name><isDefault>false</isDefault><value>1</value>\
                      </enumeratedValue>\
                      <enumeratedValue><name>C</name><isDefault>yes</isDefault><value>2</value>\
                      </enumeratedValue>";
        let (device, diags) = ok(&one_field(&set("", values)));
        let values = &reg(&periph(&device, "P").items, "R").fields[0].enums[0].values;
        let flags: Vec<_> = values.iter().map(|v| v.is_default).collect();
        assert_eq!(flags, [true, false, false]);
        assert_eq!(codes(&diags), ["svd::invalid-value"]);
    }

    // ---- other elements of registers, fields and enumerated values

    #[test]
    fn markers_alternates_and_enumerated_values_are_read() {
        let values = "<enumeratedValue><name>A</name><description>a</description>\
                      <value>#1x0</value></enumeratedValue>\
                      <enumeratedValue><isDefault>true</isDefault></enumeratedValue>\
                      <enumeratedValue><name>B</name><value>0x3</value></enumeratedValue>";
        let writes = "<writeConstraint><writeAsRead>true</writeAsRead></writeConstraint>";
        let text = svd(
            "",
            &p(
                "P",
                &regs(&format!(
                    "{}{}<cluster><name>C</name><addressOffset>0x10</addressOffset>\
                     <alternateCluster>OTHER</alternateCluster></cluster>",
                    r(
                        "R",
                        &format!(
                            "<alternateGroup> G </alternateGroup>\
                             <alternateRegister>S</alternateRegister>{writes}\
                             <protection><secure>s</secure></protection>"
                        )
                    ),
                    r(
                        "T",
                        &fields(&f("F", &format!("{writes}{}", set("", values))))
                    )
                )),
            ),
        );
        let (device, _) = ok(&text);
        let items = &periph(&device, "P").items;
        let register = reg(items, "R");
        assert_eq!(register.alternate_group.as_deref(), Some("G"));
        assert_eq!(register.alternate_register.as_deref(), Some("S"));
        assert_eq!(&text[register.write_constraint.unwrap().range()], writes);
        assert!(text[register.protection.unwrap().range()].starts_with("<protection>"));
        assert_eq!(
            cluster(items, "C").alternate_cluster.as_deref(),
            Some("OTHER")
        );
        let field = &reg(items, "T").fields[0];
        assert!(field.write_constraint.is_some());
        let values = &field.enums[0].values;
        assert_eq!(
            values[0].value,
            Some(EnumNumber::DontCare("#1x0".to_owned()))
        );
        assert_eq!(values[0].description.as_deref(), Some("a"));
        assert_eq!(values[1].name, None);
        assert!(values[1].is_default && values[1].value.is_none());
        assert_eq!(values[2].value, Some(EnumNumber::Value(3)));
        assert!(text[values[2].span.range()].starts_with("<enumeratedValue>"));
    }

    #[test]
    fn an_enumerated_value_may_have_no_name() {
        let values = "<enumeratedValue><value>1</value></enumeratedValue>\
                      <enumeratedValue><name></name><value>2</value></enumeratedValue>";
        let (device, _) = ok(&one_field(&set("", values)));
        let set = &reg(&periph(&device, "P").items, "R").fields[0].enums[0];
        assert_eq!(set.values.len(), 2);
        assert!(set.values.iter().all(|v| v.name.is_none()));
    }

    #[test]
    fn the_sets_of_a_model_have_distinct_ids() {
        let text = svd(
            "",
            &format!(
                "{}<peripheral derivedFrom=\"A\"><name>B</name><size>16</size></peripheral>",
                p(
                    "A",
                    &regs(&r("R", &fields(&f("F", &set("", &value("A", "0"))))))
                )
            ),
        );
        let (device, _) = ok(&text);
        let id = |name: &str| reg(&periph(&device, name).items, "R").fields[0].enums[0].id;
        assert_ne!(id("A"), id("B"));
    }

    #[test]
    fn an_xml_error_in_a_file_with_a_bom_is_at_the_same_place() {
        let plain = "<a>\n<b>é</c>\n</a>";
        let with_bom = format!("\u{FEFF}{plain}");
        let at = |text: &str| {
            let (device, diags) = read_str(text);
            assert!(device.is_none());
            assert_eq!(codes(&diags), ["svd::xml"]);
            diags[0].labels[0].span
        };
        let (a, b) = (at(plain), at(&with_bom));
        assert_eq!(b.lo, a.lo + 3, "the BOM is three bytes");
        assert_eq!(&with_bom[b.range()], &plain[a.range()]);
        // On the first line, the BOM counts as a character of the column.
        let first = "\u{FEFF}<a></c>".to_owned();
        let b = at(&first);
        assert!(first.is_char_boundary(b.lo as usize));
        assert!(first[..b.lo as usize].starts_with("\u{FEFF}<a>"));
    }

    #[test]
    fn a_chain_of_derived_elements_that_doubles_the_tree_is_refused() {
        // Each cluster copies the children of the one before, and holds
        // a copy of that one as well: the tree doubles at every level.
        let mut clusters = format!(
            "<cluster><name>C0</name><addressOffset>0</addressOffset>{}</cluster>",
            r("X", "")
        );
        for k in 1..=40 {
            clusters += &format!(
                "<cluster derivedFrom=\"C{prev}\"><name>C{k}</name>\
                 <cluster derivedFrom=\"P.C{prev}\"><name>D{k}</name></cluster></cluster>",
                prev = k - 1
            );
        }
        let text = svd("", &p("P", &regs(&clusters)));
        assert!(text.len() < 8 * 1024, "a few kilobytes: {}", text.len());
        let (device, diags) = read_str(&text);
        assert!(device.is_none());
        assert!(!diags.is_empty());
        assert!(
            codes(&diags).iter().all(|c| *c == "svd::too-large"),
            "{diags:#?}"
        );
        let label = diags[0].labels[0].span;
        assert!(text[label.range()].starts_with('C') || text[label.range()].starts_with("P.C"));
    }

    #[test]
    fn a_derived_tree_just_under_the_ceiling_is_copied() {
        let mut clusters = format!(
            "<cluster><name>C0</name><addressOffset>0</addressOffset>{}</cluster>",
            r("X", "")
        );
        for k in 1..=10 {
            clusters += &format!(
                "<cluster derivedFrom=\"C{prev}\"><name>C{k}</name>\
                 <cluster derivedFrom=\"P.C{prev}\"><name>D{k}</name></cluster></cluster>",
                prev = k - 1
            );
        }
        let (device, diags) = ok(&svd("", &p("P", &regs(&clusters))));
        assert!(diags.is_empty());
        assert_eq!(names(&periph(&device, "P").items).len(), 11);
    }

    /// Runs `body` on a thread with a small stack (half the main thread's
    /// on Windows), so that a recursion the caps do not bound overflows.
    fn on_a_small_stack(body: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(body)
            .expect("a thread starts")
            .join()
            .expect("the body does not overflow or panic");
    }

    /// `n` clusters inside one another, the innermost holding a register
    /// with a field and a set (the deepest an element may stand).
    fn nested_clusters(n: usize) -> String {
        let set = set("", &value("A", "0"));
        let inner = r("X", &fields(&f("F", &set)));
        let open = "<cluster><name>C</name><addressOffset>0</addressOffset>".repeat(n);
        let close = "</cluster>".repeat(n);
        svd("", &p("P", &regs(&format!("{open}{inner}{close}"))))
    }

    #[test]
    fn clusters_nested_past_the_cap_are_refused_and_at_it_are_read() {
        on_a_small_stack(|| {
            // Peripheral 1, 28 clusters, register 30, field 31, set 32.
            let (device, diags) = ok(&nested_clusters(28));
            assert!(diags.is_empty());
            assert_eq!(device.peripherals.len(), 1);
            for n in [29, 400, 100_000] {
                let text = nested_clusters(n);
                let (device, diags) = read_str(&text);
                assert!(device.is_none(), "{n}");
                assert_eq!(codes(&diags), ["svd::too-large"], "{n}");
                assert!(text[diags[0].labels[0].span.range()].starts_with("<cluster>"));
            }
        });
    }

    /// Clusters `C0` to `Cn`; `Ck` holds `Dk`, a copy of `C(k-1)`, so that
    /// `Ck` is one level deeper than `C(k-1)`.
    fn linear_clusters(n: usize) -> String {
        let mut clusters = format!(
            "<cluster><name>C0</name><addressOffset>0</addressOffset>{}</cluster>",
            r("X", "")
        );
        for k in 1..=n {
            clusters += &format!(
                "<cluster><name>C{k}</name><addressOffset>0</addressOffset>\
                 <cluster derivedFrom=\"P.C{prev}\"><name>D{k}</name></cluster></cluster>",
                prev = k - 1
            );
        }
        svd("", &p("P", &regs(&clusters)))
    }

    #[test]
    fn a_chain_of_derived_clusters_that_goes_deeper_than_the_cap_is_refused() {
        on_a_small_stack(|| {
            // `C29` is 30 levels high below the peripheral's register
            // level: its deepest element stands at depth 32.
            let (device, diags) = ok(&linear_clusters(29));
            assert!(diags.is_empty());
            assert_eq!(names(&periph(&device, "P").items).len(), 30);
            for n in [30, 200, 3000] {
                let text = linear_clusters(n);
                let (device, diags) = read_str(&text);
                assert!(device.is_none(), "{n}");
                assert!(!diags.is_empty());
                assert!(codes(&diags).iter().all(|c| *c == "svd::too-large"), "{n}");
            }
        });
    }

    /// Peripherals `A0` to `An`, each derived from the one before and
    /// written last first, so that resolving the first waits on all.
    fn peripheral_chain(n: usize) -> String {
        let mut peripherals = String::new();
        for k in (1..=n).rev() {
            peripherals += &format!(
                "<peripheral derivedFrom=\"A{prev}\"><name>A{k}</name></peripheral>",
                prev = k - 1
            );
        }
        peripherals += &p("A0", &regs(&r("R", "")));
        svd("", &peripherals)
    }

    #[test]
    fn a_chain_of_derivedfrom_longer_than_the_cap_is_refused() {
        on_a_small_stack(|| {
            // 32 elements wait on one another: the cap.
            let (device, _) = ok(&peripheral_chain(32));
            assert_eq!(device.peripherals.len(), 33);
            for n in [33, 100, 3000] {
                let (device, diags) = read_str(&peripheral_chain(n));
                assert!(device.is_none(), "{n}");
                assert!(!diags.is_empty());
                assert!(codes(&diags).iter().all(|c| *c == "svd::too-large"), "{n}");
            }
        });
    }

    #[test]
    fn the_caps_together_stay_within_a_small_stack() {
        // The worst the caps allow: a chain of 32 registers, each at the
        // bottom of 25 nested clusters of its own peripheral, deriving
        // from the one before by a path, written last first.
        on_a_small_stack(|| {
            let path = |k: usize| format!("P{k}{}.X", ".C".repeat(25));
            let cluster_open = "<cluster><name>C</name><addressOffset>0</addressOffset>".repeat(25);
            let cluster_close = "</cluster>".repeat(25);
            let mut peripherals = String::new();
            for k in (0..32).rev() {
                let register = if k == 0 {
                    r("X", "")
                } else {
                    format!(
                        "<register derivedFrom=\"{}\"><name>X</name></register>",
                        path(k - 1)
                    )
                };
                peripherals += &p(
                    &format!("P{k}"),
                    &regs(&format!("{cluster_open}{register}{cluster_close}")),
                );
            }
            let (device, diags) = ok(&svd("", &peripherals));
            assert!(diags.is_empty());
            assert_eq!(device.peripherals.len(), 32);
        });
    }
}
