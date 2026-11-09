//! Draws the README animation: a small shopping-cart store, its live
//! dependency graph (from `Store::inspect`) and its undo history.
//!
//! ```sh
//! cargo run --example graph -- /tmp/frames
//! convert -delay 4 -loop 0 /tmp/frames/frame-*.svg -layers Optimize docs/graph.gif
//! ```

use std::cell::RefCell;
use std::fmt::Write;
use std::rc::Rc;
use std::{env, fs, path::PathBuf};

use weaver_store::{NodeKind, Store};

const W: f32 = 760.0;
const H: f32 = 420.0;

struct Look {
    glow: Vec<f32>,
    calm: Vec<f32>,
    seen: Vec<(u64, u64)>,
}

fn main() -> std::io::Result<()> {
    let out = PathBuf::from(env::args().nth(1).unwrap_or_else(|| "target/frames".into()));
    fs::create_dir_all(&out)?;

    let store = Store::new();
    let price = store.signal(4.0f64);
    let qty = store.signal(3u32);
    let discount = store.signal(0u32);
    let subtotal = store.computed(move |s| s.get(price) * s.get(qty) as f64);
    let total = store.computed(move |s| s.get(subtotal) * (100 - s.get(discount)) as f64 / 100.0);
    let free = store.computed(move |s| s.get(total) >= 20.0);
    let title = Rc::new(RefCell::new(String::new()));
    let badge = Rc::new(RefCell::new(String::new()));
    let (t, b) = (title.clone(), badge.clone());
    let e1 = store.effect(move |s| *t.borrow_mut() = format!("${:.2}", s.get(total)));
    let e2 = store.effect(move |s| {
        *b.borrow_mut() = if s.get(free) {
            "free shipping"
        } else {
            "shipping $5"
        }
        .into()
    });
    for (id, name) in [
        (usize::from(price), "price"),
        (qty.into(), "qty"),
        (discount.into(), "discount"),
        (subtotal.into(), "subtotal"),
        (total.into(), "total"),
        (free.into(), "free_shipping"),
        (e1.into(), "show total"),
        (e2.into(), "show badge"),
    ] {
        store.name(id, name);
    }

    let n = store.inspect().len();
    let mut look = Look {
        glow: vec![0.0; n],
        calm: vec![0.0; n],
        seen: store
            .inspect()
            .iter()
            .map(|i| (i.version, i.runs))
            .collect(),
    };
    // Node positions: signals, computed values, effects.
    let pos = [
        (40.0, 66.0),
        (40.0, 166.0),
        (40.0, 266.0),
        (230.0, 66.0),
        (230.0, 166.0),
        (230.0, 266.0),
        (420.0, 166.0),
        (420.0, 266.0),
    ];
    let values = |store: &Store| -> Vec<String> {
        vec![
            format!("{:.2}", store.peek(price)),
            store.peek(qty).to_string(),
            format!("{}%", store.peek(discount)),
            format!("{:.2}", store.peek(subtotal)),
            format!("{:.2}", store.peek(total)),
            store.peek(free).to_string(),
            title.borrow().clone(),
            badge.borrow().clone(),
        ]
    };

    type Action = Box<dyn Fn(&Store)>;
    let script: Vec<(&str, Action)> = vec![
        ("set(qty, 4)", Box::new(move |s: &Store| s.set(qty, 4))),
        ("set(qty, 5)", Box::new(move |s: &Store| s.set(qty, 5))),
        (
            "transaction(\"coupon\")",
            Box::new(move |s: &Store| {
                s.transaction("coupon", |s| {
                    s.set(discount, 10);
                    s.set(price, 4.5);
                })
            }),
        ),
        (
            "undo()",
            Box::new(|s: &Store| {
                s.undo();
            }),
        ),
        (
            "undo()",
            Box::new(|s: &Store| {
                s.undo();
            }),
        ),
        (
            "redo()",
            Box::new(|s: &Store| {
                s.redo();
            }),
        ),
    ];

    let mut frame_no = 0;
    let mut caption = String::from("Store::new()");
    let mut emit = |store: &Store, look: &mut Look, caption: &str| -> std::io::Result<()> {
        let svg = draw(store, look, &pos, &values(store), caption);
        fs::write(out.join(format!("frame-{frame_no:04}.svg")), svg)?;
        frame_no += 1;
        for g in look.glow.iter_mut().chain(look.calm.iter_mut()) {
            *g = (*g - 0.045).max(0.0);
        }
        Ok(())
    };
    for _ in 0..15 {
        emit(&store, &mut look, &caption)?;
    }
    for (label, action) in &script {
        caption = label.to_string();
        action(&store);
        for (i, info) in store.inspect().iter().enumerate() {
            let (v, r) = look.seen[i];
            let changed = match info.kind {
                NodeKind::Effect => info.runs != r,
                _ => info.version != v,
            };
            if changed {
                look.glow[i] = 1.0;
            } else if info.runs != r {
                look.calm[i] = 1.0;
            }
            look.seen[i] = (info.version, info.runs);
        }
        for _ in 0..30 {
            emit(&store, &mut look, &caption)?;
        }
    }
    println!("wrote frames to {}", out.display());
    Ok(())
}

fn draw(
    store: &Store,
    look: &Look,
    pos: &[(f32, f32)],
    values: &[String],
    caption: &str,
) -> String {
    let (nw, nh) = (150.0, 52.0);
    let mut s = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" font-family="DejaVu Sans, Inter, Helvetica, sans-serif"><rect width="{W}" height="{H}" fill="#0f1117"/>"##
    );
    let heads = [(40.0, "SIGNALS"), (230.0, "COMPUTED"), (420.0, "EFFECTS")];
    for (x, h) in heads {
        let _ = write!(
            s,
            r##"<text x="{x}" y="44" font-size="11" font-weight="700" fill="#8b93a7">{h}</text>"##
        );
    }
    let infos = store.inspect();
    // Edges.
    for info in &infos {
        for &d in &info.deps {
            let same_column = pos[d].0 == pos[info.id].0;
            let g = look.glow[info.id].min(look.glow[d].max(0.0) + 0.3)
                * (look.glow[info.id] > 0.0) as u8 as f32;
            let color = if g > 0.05 { "#6c8cff" } else { "#2a3040" };
            let width = 1.5 + 2.0 * g;
            let path = if same_column {
                let x = pos[d].0 + nw / 2.0;
                format!("M{x} {} L{x} {}", pos[d].1 + nh, pos[info.id].1)
            } else {
                let (x1, y1) = (pos[d].0 + nw, pos[d].1 + nh / 2.0);
                let (x2, y2) = (pos[info.id].0, pos[info.id].1 + nh / 2.0);
                let mx = (x1 + x2) / 2.0;
                format!("M{x1} {y1} C{mx} {y1} {mx} {y2} {x2} {y2}")
            };
            let _ = write!(
                s,
                r#"<path d="{path}" fill="none" stroke="{color}" stroke-width="{width:.2}" opacity="{:.2}"/>"#,
                0.6 + 0.4 * g
            );
        }
    }
    // Nodes.
    for info in &infos {
        let (x, y) = pos[info.id];
        let g = look.glow[info.id];
        let c = look.calm[info.id];
        let accent = match info.kind {
            NodeKind::Signal => "#3ccf91",
            NodeKind::Computed => "#6c8cff",
            NodeKind::Effect => "#f5a524",
        };
        if g > 0.0 {
            let _ = write!(
                s,
                r#"<rect x="{}" y="{}" width="{}" height="{}" rx="14" fill="{accent}" opacity="{:.2}"/>"#,
                x - 5.0,
                y - 5.0,
                nw + 10.0,
                nh + 10.0,
                0.35 * g
            );
        }
        let stroke = if c > 0.0 { "#8b93a7" } else { "#252a36" };
        let _ = write!(
            s,
            r##"<rect x="{x}" y="{y}" width="{nw}" height="{nh}" rx="10" fill="#171a23" stroke="{stroke}" stroke-width="{:.1}"/><rect x="{x}" y="{}" width="4" height="{}" rx="2" fill="{accent}"/>"##,
            1.0 + c,
            y + 10.0,
            nh - 20.0
        );
        let name = info.name.as_deref().unwrap_or("");
        let _ = write!(
            s,
            r##"<text x="{}" y="{}" font-size="11" fill="#8b93a7">{name}</text><text x="{}" y="{}" font-size="15" font-weight="700" fill="#e6e9f0">{}</text>"##,
            x + 14.0,
            y + 20.0,
            x + 14.0,
            y + 40.0,
            values[info.id]
        );
        if c > 0.0 && g == 0.0 {
            let _ = write!(
                s,
                r##"<text x="{}" y="{}" font-size="10" fill="#8b93a7" text-anchor="end" opacity="{c:.2}">unchanged</text>"##,
                x + nw - 10.0,
                y + 40.0
            );
        }
    }
    // History.
    let hx = 606.0;
    let _ = write!(
        s,
        r##"<rect x="{hx}" y="56" width="138" height="{}" rx="10" fill="#151821"/><text x="{hx}" y="44" font-size="11" font-weight="700" fill="#8b93a7">UNDO HISTORY</text>"##,
        H - 120.0
    );
    let mut y = 84.0;
    for label in store.history() {
        let _ = write!(
            s,
            r##"<text x="{}" y="{y}" font-size="12" fill="#e6e9f0">{label}</text>"##,
            hx + 14.0
        );
        y += 24.0;
    }
    if let Some(redo) = store.redo_label() {
        let _ = write!(
            s,
            r##"<text x="{}" y="{y}" font-size="12" fill="#4b5263">{redo} (redo)</text>"##,
            hx + 14.0
        );
    }
    // Caption.
    let _ = write!(
        s,
        r##"<rect x="40" y="{}" width="{}" height="34" rx="8" fill="#151821"/><text x="56" y="{}" font-size="14" font-weight="700" fill="#e6e9f0" font-family="DejaVu Sans Mono, monospace">store.{caption}</text>"##,
        H - 52.0,
        W - 80.0,
        H - 30.0
    );
    s.push_str("</svg>");
    s
}
