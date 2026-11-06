fn main() {
    let set = bazza_detect::PatternSet::builtin();
    let windows = bazza_detect::list_visible_windows();
    for w in &windows {
        println!("{:<28} {}", w.process, w.title);
    }
    println!("--- detected: {:?}", set.match_windows(&windows));
}
