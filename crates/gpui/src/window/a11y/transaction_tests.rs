use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use accesskit::{Action, NodeId, Role};

use super::{A11y, ROOT_NODE_ID};
use crate::{Bounds, FocusId, point, px, size};

fn a11y() -> A11y {
    let mut a11y = A11y::new(Arc::new(AtomicBool::new(true)), false, None);
    a11y.sync_active_flag();
    a11y.begin_frame();
    a11y
}

fn node(label: &str) -> accesskit::Node {
    let mut node = accesskit::Node::new(Role::GenericContainer);
    node.set_label(label);
    node
}

#[test]
fn rollback_restores_open_ancestors_and_allows_reusing_aborted_node_ids() {
    let mut a11y = a11y();
    let parent = NodeId(1);
    let preserved = NodeId(2);
    let completed = NodeId(3);
    let open = NodeId(4);
    assert!(a11y.nodes.push(parent, node("parent")));
    assert!(a11y.nodes.push_leaf(preserved, node("preserved")));
    let checkpoint = a11y.prepaint_checkpoint();

    a11y.nodes.current_node_mut().unwrap().set_label("aborted");
    assert!(a11y.nodes.push(completed, node("aborted")));
    #[cfg(debug_assertions)]
    a11y.nodes.record_node_info(completed, Default::default());
    a11y.nodes.pop();
    assert!(a11y.nodes.push(open, node("aborted")));
    a11y.rollback_prepaint(checkpoint);

    assert_eq!(
        a11y.nodes.current_node_mut().unwrap().label(),
        Some("parent")
    );
    assert_eq!(
        a11y.nodes.current_node_mut().unwrap().children(),
        &[preserved]
    );
    assert!(a11y.nodes.has_node(preserved));
    assert!(!a11y.nodes.has_node(completed));
    assert!(!a11y.nodes.has_node(open));
    #[cfg(debug_assertions)]
    assert!(!a11y.nodes.node_info.contains_key(&completed));
    assert!(a11y.nodes.push_leaf(completed, node("retried")));
    assert!(a11y.nodes.push_leaf(open, node("retried")));
    a11y.nodes.pop();
    let update = a11y.end_frame(Default::default());
    assert_eq!(update.nodes.len(), 5);
    let parent = &update.nodes.iter().find(|(id, _)| *id == parent).unwrap().1;
    assert_eq!(parent.children(), &[preserved, completed, open]);
}

#[test]
fn rollback_restores_focus_bounds_and_active_descendant_before_retry() {
    let mut a11y = a11y();
    let container = NodeId(1);
    let child = NodeId(2);
    let original_bounds = Bounds::new(point(px(0.), px(0.)), size(px(10.), px(10.)));
    assert!(a11y.nodes.push(container, node("container")));
    a11y.set_focusable(container, FocusId::default());
    a11y.node_bounds.insert(container, original_bounds);
    let checkpoint = a11y.prepaint_checkpoint();

    a11y.set_focus(container);
    assert!(a11y.nodes.push(child, node("aborted")));
    a11y.set_focusable(child, FocusId::default());
    a11y.node_bounds.insert(child, original_bounds);
    a11y.node_bounds.insert(container, Bounds::default());
    a11y.set_active_descendant(child);
    a11y.nodes.pop();
    a11y.last_focus_without_node = Some(FocusId::default());
    a11y.rollback_prepaint(checkpoint);

    assert_eq!(a11y.nodes.focus, None);
    assert_eq!(a11y.nodes.active_descendant, None);
    assert_eq!(a11y.last_focus_without_node, None);
    assert_eq!(a11y.node_bounds.get(&container), Some(&original_bounds));
    assert!(!a11y.node_bounds.contains_key(&child));
    assert!(!a11y.focus_ids.contains_key(&child));
    a11y.set_focus(container);
    assert!(a11y.nodes.push(child, node("retried")));
    a11y.set_active_descendant(child);
    a11y.nodes.pop();
    a11y.nodes.pop();
    assert_eq!(a11y.end_frame(Default::default()).focus, child);
}

#[test]
fn nested_rollback_preserves_outer_nodes_and_existing_focus() {
    let mut a11y = a11y();
    let focused = NodeId(1);
    assert!(a11y.nodes.push_leaf(focused, node("focused")));
    a11y.set_focusable(focused, FocusId::default());
    a11y.set_focus(focused);
    let outer = a11y.prepaint_checkpoint();
    assert!(a11y.nodes.push_leaf(NodeId(2), node("outer")));
    let inner = a11y.prepaint_checkpoint();
    assert!(a11y.nodes.push_leaf(NodeId(3), node("inner")));
    a11y.rollback_prepaint(inner);
    assert!(a11y.nodes.has_node(NodeId(2)));
    assert!(!a11y.nodes.has_node(NodeId(3)));
    assert_eq!(a11y.nodes.focus, Some(focused));
    // A successful inner transaction is still part of an aborted outer one.
    let committed_inner = a11y.prepaint_checkpoint();
    assert!(a11y.nodes.push_leaf(NodeId(4), node("committed inner")));
    drop(committed_inner);
    a11y.rollback_prepaint(outer);
    assert!(!a11y.nodes.has_node(NodeId(2)));
    assert!(!a11y.nodes.has_node(NodeId(4)));
    assert_eq!(a11y.nodes.focus, Some(focused));
    assert_eq!(a11y.end_frame(Default::default()).nodes.len(), 2);
}

#[test]
fn rollback_drops_only_listeners_registered_after_the_checkpoint() {
    struct Dropped(Arc<AtomicUsize>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let mut a11y = a11y();
    a11y.action_listeners
        .insert(NodeId(1), vec![(Action::Focus, Box::new(|_, _, _| {}))]);
    let checkpoint = a11y.prepaint_checkpoint();
    let dropped = Arc::new(AtomicUsize::new(0));
    for id in [NodeId(1), NodeId(2)] {
        let tracked = Dropped(dropped.clone());
        a11y.action_listeners.entry(id).or_default().push((
            Action::Click,
            Box::new(move |_, _, _| {
                let _keep_alive = &tracked;
            }),
        ));
    }
    a11y.rollback_prepaint(checkpoint);
    assert_eq!(dropped.load(Ordering::Relaxed), 2);
    assert_eq!(a11y.action_listeners.len(), 1);
    assert_eq!(a11y.action_listeners[&NodeId(1)].len(), 1);
    assert_eq!(a11y.action_listeners[&NodeId(1)][0].0, Action::Focus);
}

#[test]
fn committing_keeps_nodes_and_inactive_checkpoint_does_not_touch_the_builder() {
    let mut a11y = a11y();
    let checkpoint = a11y.prepaint_checkpoint();
    assert!(a11y.nodes.push_leaf(NodeId(1), node("committed")));
    drop(checkpoint);
    assert_eq!(a11y.end_frame(Default::default()).nodes.len(), 2);

    a11y.active_this_frame = false;
    let checkpoint = a11y.prepaint_checkpoint();
    assert!(checkpoint.nodes.is_none());
    a11y.rollback_prepaint(checkpoint);
    assert!(a11y.nodes.has_node(NodeId(1)));
    assert!(a11y.nodes.has_node(ROOT_NODE_ID));
}

#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "Duplicate a11y node id"))]
fn rollback_does_not_hide_real_duplicates_in_the_retried_transaction() {
    let mut a11y = a11y();
    let checkpoint = a11y.prepaint_checkpoint();
    assert!(a11y.nodes.push_leaf(NodeId(1), node("aborted")));
    a11y.rollback_prepaint(checkpoint);
    assert!(a11y.nodes.push_leaf(NodeId(1), node("retried")));
    assert!(!a11y.nodes.push_leaf(NodeId(1), node("duplicate")));
}
