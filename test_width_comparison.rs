use unicode_width::UnicodeWidthStr;
use unicode_display_width::width;

fn main() {
    let test_cases = vec![
        "🟢",
        "⬇️1", 
        "⬆️12",
        "⚠️git",
        "🔀3↑2↓",
        "📍",
        "❓",
        "📝"
    ];
    
    println!("String         | unicode-width | unicode-display-width");
    println!("---------------|---------------|----------------------");
    
    for case in test_cases {
        let uw = case.width();
        let udw = width(case);
        println!("{:<14} | {:<13} | {}", case, uw, udw);
    }
}
