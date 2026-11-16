use taffy::prelude::*;

#[test]
fn gaps_stay_between_items_when_an_auto_margin_takes_the_free_space() {
    let mut tree: TaffyTree<()> = TaffyTree::new();
    let fixed = |width: f32| Style { size: Size { width: length(width), height: length(16.0_f32) }, ..Default::default() };
    let icon = tree.new_leaf(fixed(16.0)).unwrap();
    let title = tree.new_leaf(fixed(50.0)).unwrap();
    let badge = tree.new_leaf(Style { margin: Rect { left: auto(), right: zero(), top: zero(), bottom: zero() }, ..fixed(18.0) }).unwrap();
    let row = tree
        .new_with_children(Style { size: Size { width: length(140.0_f32), height: length(40.0_f32) }, gap: Size { width: length(7.0_f32), height: zero() }, ..Default::default() }, &[icon, title, badge])
        .unwrap();
    tree.compute_layout(row, Size::MAX_CONTENT).unwrap();
    assert_eq!(tree.layout(title).unwrap().location.x, 23.0);
    assert_eq!(tree.layout(badge).unwrap().location.x, 122.0);
}

#[test]
fn a_box_snaps_where_it_lands_in_the_window_rather_than_by_its_rounded_ancestors() {
    let mut tree: TaffyTree<()> = TaffyTree::new();
    let inner = tree.new_leaf(Style { size: Size { width: length(10.0_f32), height: length(10.0_f32) }, margin: Rect { left: length(0.4_f32), right: zero(), top: zero(), bottom: zero() }, ..Default::default() }).unwrap();
    let middle = tree.new_with_children(Style { margin: Rect { left: length(0.4_f32), right: zero(), top: zero(), bottom: zero() }, ..Default::default() }, &[inner]).unwrap();
    let outer = tree.new_with_children(Style { size: Size { width: length(100.0_f32), height: length(20.0_f32) }, ..Default::default() }, &[middle]).unwrap();
    tree.compute_layout(outer, Size::MAX_CONTENT).unwrap();
    // 0.4 + 0.4 lands at 0.8, which snaps to 1, though each offset alone rounds to 0.
    let landed = tree.layout(middle).unwrap().location.x + tree.layout(inner).unwrap().location.x;
    assert_eq!(landed, 1.0);
}

#[test]
fn a_column_sized_by_its_content_counts_an_unshrinkable_item_pulled_up_by_a_negative_margin() {
    let mut tree: TaffyTree<()> = TaffyTree::new();
    let search = tree
        .new_leaf(Style { flex_shrink: 0.0, size: Size { width: auto(), height: length(38.0_f32) }, margin: Rect { left: zero(), right: zero(), top: length(-8.0_f32), bottom: zero() }, ..Default::default() })
        .unwrap();
    let grid = tree.new_leaf(Style { size: Size { width: auto(), height: length(268.0_f32) }, ..Default::default() }).unwrap();
    let column = tree
        .new_with_children(Style { display: Display::Flex, flex_direction: FlexDirection::Column, size: Size { width: length(320.0_f32), height: auto() }, gap: Size { width: zero(), height: length(9.0_f32) }, ..Default::default() }, &[search, grid])
        .unwrap();
    let block = tree.new_with_children(Style { display: Display::Block, ..Default::default() }, &[column]).unwrap();
    tree.compute_layout(block, Size::MAX_CONTENT).unwrap();
    assert_eq!(tree.layout(column).unwrap().size.height, 38.0 - 8.0 + 9.0 + 268.0);
}
