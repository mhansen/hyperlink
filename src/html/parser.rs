use bumpalo::collections::String as BumpString;
use bumpalo::collections::Vec as BumpVec;
use bumpalo::Bump;
use html5gum::{Emitter, Error};

use crate::html::{DefinedLink, Document, Link, UsedLink};
use crate::paragraph::ParagraphWalker;

#[inline]
fn is_paragraph_tag(tag: &[u8]) -> bool {
    tag == b"p" || tag == b"li" || tag == b"dt" || tag == b"dd"
}

#[inline]
fn try_normalize_href_value(input: &str) -> &str {
    input.trim()
}

#[inline]
fn is_bad_schema(url: &[u8]) -> bool {
    // check if url is empty
    let first_char = match url.first() {
        Some(x) => x,
        None => return false,
    };

    // protocol-relative URL
    if url.starts_with(b"//") {
        return true;
    }

    // check if string before first : is a valid URL scheme
    // see RFC 2396, Appendix A for what constitutes a valid scheme

    if !matches!(first_char, b'a'..=b'z' | b'A'..=b'Z') {
        return false;
    }

    for c in &url[1..] {
        match c {
            b'a'..=b'z' => (),
            b'A'..=b'Z' => (),
            b'0'..=b'9' => (),
            b'+' => (),
            b'-' => (),
            b'.' => (),
            b':' => return true,
            _ => return false,
        }
    }

    false
}

#[derive(Default)]
pub struct ParserBuffers {
    current_tag_name: String,
    current_attribute_name: String,
    current_attribute_value: String,
    last_start_tag: String,
}

impl ParserBuffers {
    pub fn reset(&mut self) {
        self.current_tag_name.clear();
        self.current_attribute_name.clear();
        self.current_attribute_value.clear();
        self.last_start_tag.clear();
    }
}

pub struct HyperlinkEmitter<'a, 'l, 'd, P: ParagraphWalker> {
    pub paragraph_walker: P,
    pub arena: &'a Bump,
    pub document: &'d Document,
    pub link_buf: &'d mut BumpVec<'a, Link<'l, P::Paragraph>>,
    pub in_paragraph: bool,
    pub last_paragraph_i: usize,
    pub get_paragraphs: bool,
    pub buffers: &'d mut ParserBuffers,
    pub current_tag_is_closing: bool,
    pub check_anchors: bool,
}

impl<'a, 'l, 'd, P> HyperlinkEmitter<'a, 'l, 'd, P>
where
    'a: 'l,
    P: ParagraphWalker,
{
    fn extract_used_link(&mut self) {
        let value = try_normalize_href_value(&self.buffers.current_attribute_value);

        if is_bad_schema(value.as_bytes()) {
            return;
        }

        self.link_buf.push(Link::Uses(UsedLink {
            href: self.document.join(self.arena, self.check_anchors, value),
            path: self.document.path.clone(),
            paragraph: None,
        }));
    }

    fn extract_used_link_srcset(&mut self) {
        let value = try_normalize_href_value(&self.buffers.current_attribute_value);

        // https://html.spec.whatwg.org/multipage/images.html#srcset-attribute
        for value in value
            .split(',')
            .filter_map(|candidate: &str| candidate.split_whitespace().next())
            .filter(|value| !value.is_empty())
        {
            if is_bad_schema(value.as_bytes()) {
                continue;
            }

            self.link_buf.push(Link::Uses(UsedLink {
                href: self.document.join(self.arena, self.check_anchors, value),
                path: self.document.path.clone(),
                paragraph: None,
            }));
        }
    }

    fn extract_anchor_def(&mut self) {
        if self.check_anchors {
            let mut href = BumpString::new_in(self.arena);
            let value = try_normalize_href_value(&self.buffers.current_attribute_value);
            href.push('#');
            href.push_str(value);

            self.link_buf.push(Link::Defines(DefinedLink {
                href: self.document.join(self.arena, self.check_anchors, &href),
            }));
        }
    }

    fn flush_old_attribute(&mut self) {
        match (
            self.buffers.current_tag_name.as_str(),
            self.buffers.current_attribute_name.as_str(),
        ) {
            ("link" | "area" | "a", "href") => self.extract_used_link(),
            ("a", "name") => self.extract_anchor_def(),
            ("img" | "script" | "iframe", "src") => self.extract_used_link(),
            ("img", "srcset") => self.extract_used_link_srcset(),
            ("object", "data") => self.extract_used_link(),
            (_, "id") => self.extract_anchor_def(),
            _ => (),
        }

        self.buffers.current_attribute_name.clear();
        self.buffers.current_attribute_value.clear();
    }
}

impl<'a, 'l, 'd, P> Emitter for HyperlinkEmitter<'a, 'l, 'd, P>
where
    'a: 'l,
    P: ParagraphWalker,
{
    type Token = ();

    fn set_last_start_tag(&mut self, last_start_tag: Option<&str>) {
        self.buffers.last_start_tag.clear();
        self.buffers
            .last_start_tag
            .push_str(last_start_tag.unwrap_or_default());
    }

    fn pop_token(&mut self) -> Option<()> {
        None
    }

    fn emit_string(&mut self, c: &str) {
        if self.get_paragraphs && self.in_paragraph {
            self.paragraph_walker.update(c.as_bytes());
        }
    }

    fn init_start_tag(&mut self) {
        self.buffers.current_tag_name.clear();
        self.current_tag_is_closing = false;
    }

    fn init_end_tag(&mut self) {
        self.buffers.current_tag_name.clear();
        self.current_tag_is_closing = true;
    }

    fn emit_current_tag(&mut self) {
        self.flush_old_attribute();

        if !self.current_tag_is_closing {
            self.buffers.last_start_tag.clear();
            self.buffers
                .last_start_tag
                .push_str(&self.buffers.current_tag_name);

            if is_paragraph_tag(self.buffers.current_tag_name.as_bytes()) {
                self.in_paragraph = true;
                self.last_paragraph_i = self.link_buf.len();
                self.paragraph_walker.finish_paragraph();
            }
        } else if is_paragraph_tag(self.buffers.current_tag_name.as_bytes()) {
            let paragraph = self.paragraph_walker.finish_paragraph();
            if self.in_paragraph {
                for link in &mut self.link_buf[self.last_paragraph_i..] {
                    match link {
                        Link::Uses(ref mut x) => {
                            x.paragraph = paragraph.clone();
                        }
                        Link::Defines(_) => (),
                    }
                }
                self.in_paragraph = false;
            }
            self.last_paragraph_i = self.link_buf.len();
        }

        self.buffers.current_tag_name.clear();
    }

    fn set_self_closing(&mut self) {
        if is_paragraph_tag(self.buffers.current_tag_name.as_bytes()) {
            self.in_paragraph = false;
        }
    }

    fn push_tag_name(&mut self, s: &str) {
        self.buffers.current_tag_name.push_str(s);
    }

    fn init_attribute(&mut self) {
        self.flush_old_attribute();
    }

    fn push_attribute_name(&mut self, s: &str) {
        self.buffers.current_attribute_name.push_str(s);
    }

    fn push_attribute_value(&mut self, s: &str) {
        self.buffers.current_attribute_value.push_str(s);
    }

    fn current_is_appropriate_end_tag_token(&mut self) -> bool {
        self.current_tag_is_closing
            && !self.buffers.current_tag_name.is_empty()
            && self.buffers.current_tag_name == self.buffers.last_start_tag
    }

    fn emit_current_comment(&mut self) {}
    fn emit_current_doctype(&mut self) {}
    fn emit_eof(&mut self) {}
    fn emit_error(&mut self, _: Error) {}
    fn init_comment(&mut self) {}
    fn init_doctype(&mut self) {}
    fn push_comment(&mut self, _: &str) {}
    fn push_doctype_name(&mut self, _: &str) {}
    fn push_doctype_public_identifier(&mut self, _: &str) {}
    fn push_doctype_system_identifier(&mut self, _: &str) {}
    fn set_doctype_public_identifier(&mut self, _: &str) {}
    fn set_doctype_system_identifier(&mut self, _: &str) {}
    fn set_force_quirks(&mut self) {}
}

#[test]
fn test_is_bad_schema() {
    assert!(is_bad_schema(b"//"));
    assert!(!is_bad_schema(b""));
    assert!(!is_bad_schema(b"http"));
    assert!(is_bad_schema(b"http:"));
    assert!(is_bad_schema(b"http:/"));
    assert!(!is_bad_schema(b"http/"));
}
