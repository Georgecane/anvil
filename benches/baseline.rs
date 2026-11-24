use anvil::deque_tree::DequeRegistry;
use anvil::runtime::{
    DequeHandle, ElementHandle, InitialDeque, InitialElement, StyleCommitAdapter, StyloStyleEngine,
    UiElement, UiModel, run_style_pass,
};
use anvil::{Deque, DequeTree};
use std::hint::black_box;
use std::time::Instant;

fn measure(mut operation: impl FnMut(), iterations: usize) -> (f64, f64) {
    for _ in 0..10_000.min(iterations) {
        operation();
    }

    let mut samples = Vec::with_capacity(7);
    for _ in 0..7 {
        let start = Instant::now();
        for _ in 0..iterations {
            operation();
        }
        samples.push(start.elapsed());
    }
    samples.sort_unstable();
    let median = samples[3].as_secs_f64() * 1e9 / iterations as f64;
    let min = samples[0].as_secs_f64() * 1e9 / iterations as f64;
    (median, min)
}

fn report(label: &str, iterations: usize, operation: impl FnMut()) {
    let (median, best) = measure(operation, iterations);
    println!("{label:54} median={median:>10.2} ns/op  best={best:>10.2} ns/op");
}

fn make_tree(size: usize) -> (DequeTree<usize>, Vec<anvil::NodeId>) {
    let mut deques = Vec::with_capacity(size);
    for index in 0..size {
        let mut deque = Deque::new();
        deque.push_back(index);
        deques.push(deque);
    }
    let tree = DequeTree::from_level_order(deques).expect("valid complete level-order tree");
    let ids = tree.nodes().map(|(id, _)| id).collect();
    (tree, ids)
}

fn make_ui_with_siblings(size: usize) -> (UiModel, ElementHandle) {
    let mut elements = Vec::with_capacity(size + 1);
    for index in 0..size {
        elements.push(InitialElement {
            handle: ElementHandle(index as u64 + 1),
            element: UiElement::new("div"),
            subtree_span: 1,
        });
    }
    elements.push(InitialElement {
        handle: ElementHandle(size as u64 + 1),
        element: UiElement::new("main"),
        subtree_span: size + 1,
    });
    let ui = UiModel::from_initial_groups(vec![InitialDeque {
        handle: DequeHandle(1),
        elements,
    }])
    .expect("valid initial UI snapshot");
    (ui, ElementHandle(size as u64 / 2 + 1))
}

fn main() {
    println!("Anvil baseline microbenchmarks");
    println!("target={} profile=bench", std::env::consts::ARCH);
    println!("Each result: median and best of 7 samples; setup excluded unless stated.");
    println!();

    for size in [127usize, 511, 2_047, 8_191] {
        let (mut tree, ids) = make_tree(size);
        let mut registry = DequeRegistry::build(&mut tree);
        let candidate = ids[size / 2];
        let iterations = (2_000_000 / size).clamp(2_000, 50_000);

        report(
            &format!("registry.detect_changes_for / {size} nodes"),
            iterations,
            || {
                black_box(registry.detect_changes_for(&mut tree, black_box([candidate])));
            },
        );

        report(
            &format!("registry.prepare_render_pending / {size} nodes"),
            iterations,
            || {
                black_box(registry.prepare_render_pending());
            },
        );

        let stack = registry.render_stack().clone();
        report(
            &format!("render_stack.full traversal / {size} nodes"),
            (100_000 / size).clamp(500, 10_000),
            || {
                let nodes = black_box(stack.as_slice());
                let checksum = nodes.iter().fold(0usize, |sum, id| {
                    sum.wrapping_add(black_box(id.index() as usize))
                });
                black_box(checksum);
            },
        );

        report(
            &format!("tree.preorder rebuild / {size} nodes"),
            (20_000 / size).clamp(100, 2_000),
            || {
                black_box(tree.preorder());
            },
        );
        println!();
    }

    for size in [128usize, 512, 1_024] {
        let mut deque = Deque::new();
        let root = deque.push_back(0usize);
        let mut leaves = Vec::with_capacity(size);
        for index in 0..size {
            leaves.push(deque.push_front(index + 1));
        }
        let leaf = leaves[size / 2];

        report(&format!("deque.index_of / {size} entries"), 100_000, || {
            black_box(deque.index_of(black_box(leaf)));
        });
        report(
            &format!("deque.structural_range(leaf) / {size} entries"),
            100_000,
            || {
                black_box(deque.structural_range(black_box(leaf)));
            },
        );
        report(
            &format!("deque.logical_parent / {size} entries"),
            100_000,
            || {
                black_box(deque.logical_parent(black_box(leaf)));
            },
        );
        report(
            &format!("deque.structural_range(root) / {size} entries"),
            100_000,
            || {
                black_box(deque.structural_range(black_box(root)));
            },
        );
        println!();
    }

    for size in [128usize, 512, 2_048, 4_096] {
        let (mut ui, target) = make_ui_with_siblings(size);
        let iterations = (100_000 / size).clamp(20, 500);
        let mut toggle = false;
        report(
            &format!("ui.set_property / {size} sibling elements"),
            iterations,
            || {
                toggle = !toggle;
                ui.set_property(target, "data-state", if toggle { "a" } else { "b" })
                    .unwrap();
                black_box(ui.element(target));
            },
        );
        println!();
    }

    for size in [128usize, 512, 2_048, 4_096] {
        let (mut ui, target) = make_ui_with_siblings(size);
        let mut engine = StyloStyleEngine::new().unwrap();
        engine.add_stylesheet("div { color: red; } .active { color: blue; }");
        let mut adapter = StyleCommitAdapter;
        // Drain the bounded initial stylesheet work before timing incremental
        // attribute invalidation, otherwise the benchmark measures that work.
        loop {
            run_style_pass(&mut engine, &mut adapter, &mut ui).unwrap();
            if engine.last_recomputed_count() == 0 {
                break;
            }
        }
        let iterations = 10;
        let mut toggle = false;
        report(
            &format!("attribute + Stylo style pass / {size} siblings"),
            iterations,
            || {
                toggle = !toggle;
                ui.set_property(target, "class", if toggle { "active" } else { "idle" })
                    .unwrap();
                run_style_pass(&mut engine, &mut adapter, &mut ui).unwrap();
                black_box(engine.last_recomputed_count());
            },
        );
        println!(
            "  Stylo recomputed elements in last pass: {}",
            engine.last_recomputed_count()
        );
        println!();
    }
}
