// sample.rs — multi-language highlighting demo (Rust)
use std::collections::HashMap;

/// A point in 2D space.
#[derive(Debug, Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }

    fn distance(&self, other: &Point) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

fn main() {
    let origin = Point::new(0.0, 0.0);
    let p = Point::new(3.0, 4.0);
    let mut scores: HashMap<&str, u32> = HashMap::new();
    scores.insert("alice", 42);

    let d = origin.distance(&p);
    println!("distance = {}, scores = {:?}", d, scores);

    for i in 0..3 {
        if i % 2 == 0 {
            println!("even: {}", i);
        } else {
            println!("odd:  {}", i);
        }
    }
}
