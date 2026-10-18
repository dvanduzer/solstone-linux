// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use gtk::cairo;
use gtk::prelude::*;
use resvg::usvg::{Options, Tree, fontdb};
use solstone_linux::window_control::MarkRenderDocument;
use std::sync::Arc;

static FONT_DATA: &[u8] = include_bytes!("../assets/Comfortaa-Bold.ttf");

pub fn mark_card_accessible_name(mark: &MarkRenderDocument) -> String {
    solstone_linux::copy::journal_mark_speech(
        &mark.icon1.color.name,
        &mark.icon2.color.name,
        &mark.words[0],
        &mark.words[1],
    )
}

pub fn create_font_database() -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_font_data(FONT_DATA.to_vec());
    db
}

pub fn build_mark_card_svg(mark: &MarkRenderDocument) -> String {
    let total_width = 240.0;
    let total_height = 130.0;
    let chip_y = 20.0;
    let chip1_cx = 20.0 + 24.0;
    let chip1_cy = chip_y + 24.0;
    let chip2_cx = chip1_cx + 48.0 + 11.0;
    let chip2_cy = chip_y + 24.0;

    let w1 = mark.words[0].to_lowercase();
    let w2 = mark.words[1].to_lowercase();

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{total_width}" height="{total_height}" viewBox="0 0 {total_width} {total_height}">
  <rect x="0.5" y="0.5" width="{}" height="{}" rx="12" fill="#fffdf9" stroke="#e7d8c6" stroke-width="1"/>
  <g transform="rotate({}, {chip1_cx}, {chip1_cy})">
    <rect x="{}" y="{}" width="48" height="48" rx="12" fill="{}" fill-opacity="0.12" stroke="{}" stroke-width="2"/>
    <g transform="translate({}, {})">
      <svg width="28" height="28" viewBox="0 0 28 28">
        {}
      </svg>
    </g>
  </g>
  <g transform="rotate({}, {chip2_cx}, {chip2_cy})">
    <rect x="{}" y="{}" width="48" height="48" rx="12" fill="{}" fill-opacity="0.12" stroke="{}" stroke-width="2"/>
    <g transform="translate({}, {})">
      <svg width="28" height="28" viewBox="0 0 28 28">
        {}
      </svg>
    </g>
  </g>
  <text x="{}" y="105" font-family="Comfortaa" font-size="18" text-anchor="middle">
    <tspan fill="#1A1A1A">{w1}</tspan><tspan fill="#6E6453">·</tspan><tspan fill="#1A1A1A">{w2}</tspan>
  </text>
</svg>"##,
        total_width - 1.0,
        total_height - 1.0,
        mark.icon1.rot,
        chip1_cx - 24.0,
        chip1_cy - 24.0,
        mark.icon1.color.hex,
        mark.icon1.color.hex,
        chip1_cx - 14.0,
        chip1_cy - 14.0,
        mark.icon1.svg,
        mark.icon2.rot,
        chip2_cx - 24.0,
        chip2_cy - 24.0,
        mark.icon2.color.hex,
        mark.icon2.color.hex,
        chip2_cx - 14.0,
        chip2_cy - 14.0,
        mark.icon2.svg,
        total_width / 2.0,
    )
}

pub fn render_mark_card_svg(svg_str: &str, scale: f32) -> Option<resvg::tiny_skia::Pixmap> {
    let fontdb = create_font_database();
    let opt = Options {
        fontdb: Arc::new(fontdb),
        ..Options::default()
    };
    let tree = Tree::from_str(svg_str, &opt).ok()?;
    let width = (tree.size().width() * scale).ceil() as u32;
    let height = (tree.size().height() * scale).ceil() as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(pixmap)
}

pub fn create_mark_card_widget(mark: MarkRenderDocument) -> gtk::Widget {
    let area = gtk::DrawingArea::new();
    area.set_content_width(240);
    area.set_content_height(130);

    let accessible_name = mark_card_accessible_name(&mark);
    let label = gtk::Label::new(Some(&accessible_name));
    label.set_opacity(0.0);
    label.set_can_target(false);

    let svg = build_mark_card_svg(&mark);

    area.set_draw_func(move |widget, cr, _width, _height| {
        let scale = widget.scale_factor() as f32;
        if let Some(pixmap) = render_mark_card_svg(&svg, scale) {
            let width = pixmap.width() as i32;
            let height = pixmap.height() as i32;
            let stride = width * 4;
            let mut cairo_data = pixmap.data().to_vec();
            for pixel in cairo_data.chunks_exact_mut(4) {
                let r = pixel[0];
                let g = pixel[1];
                let b = pixel[2];
                let a = pixel[3];
                pixel[0] = b;
                pixel[1] = g;
                pixel[2] = r;
                pixel[3] = a;
            }
            if let Ok(surface) = cairo::ImageSurface::create_for_data(
                cairo_data,
                cairo::Format::ARgb32,
                width,
                height,
                stride,
            ) {
                cr.save().ok();
                cr.scale(1.0 / scale as f64, 1.0 / scale as f64);
                let _ = cr.set_source_surface(&surface, 0.0, 0.0);
                let _ = cr.paint();
                cr.restore().ok();
            }
        }
    });

    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    container.append(&area);
    container.append(&label);
    container.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use solstone_linux::window_control::{MarkColorDocument, MarkIconDocument, MarkRenderDocument};

    #[test]
    fn accessible_name_matches_speech() {
        let mark = MarkRenderDocument {
            icon1: MarkIconDocument {
                name: "star".into(),
                svg: "<path d=\"M0 0 L10 10\"/>".into(),
                color: MarkColorDocument {
                    name: "gold".into(),
                    hex: "#ffd700".into(),
                },
                rot: 0,
            },
            icon2: MarkIconDocument {
                name: "moon".into(),
                svg: "<path d=\"M0 0 L10 10\"/>".into(),
                color: MarkColorDocument {
                    name: "silver".into(),
                    hex: "#c0c0c0".into(),
                },
                rot: 45,
            },
            words: ["amber".into(), "river".into()],
        };
        let expected =
            solstone_linux::copy::journal_mark_speech("gold", "silver", "amber", "river");
        assert_eq!(mark_card_accessible_name(&mark), expected);

        let svg = build_mark_card_svg(&mark);
        assert!(!svg.is_empty());
        let pixmap = render_mark_card_svg(&svg, 1.0);
        assert!(pixmap.is_some());
    }
}
