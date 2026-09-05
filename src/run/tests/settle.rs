//! When the files that trail the screen are owed a write.

use super::*;

#[test]
fn a_frame_waits_for_the_screen_to_still() {
    let start = Instant::now();
    let mut settle = Settle::default();
    assert!(!settle.due(start), "nothing drawn, nothing to write");
    assert_eq!(settle.wakeup(start), None);

    settle.drew(start);
    assert!(
        !settle.due(start),
        "the frame that dirtied it writes nothing"
    );
    assert_eq!(settle.wakeup(start), Some(Duration::from_millis(300)));
    assert_eq!(
        settle.wakeup(start + Duration::from_millis(100)),
        Some(Duration::from_millis(200)),
        "the loop wakes for what is left of the quiet spell"
    );
    assert!(settle.due(start + Duration::from_millis(300)));
}

#[test]
fn a_screen_that_never_stills_still_writes_once_a_second() {
    let start = Instant::now();
    let mut settle = Settle::default();
    // A spinner, repainting ten times a second: never quiet for 300 ms.
    for tick in 0..10 {
        let now = start + Duration::from_millis(tick * 100);
        settle.drew(now);
        assert!(!settle.due(now), "still moving at {tick}00 ms");
    }
    let now = start + Duration::from_secs(1);
    settle.drew(now);
    assert!(settle.due(now), "a second of drawing is long enough");

    settle.wrote();
    assert!(!settle.due(now + Duration::from_secs(5)));
    assert_eq!(settle.wakeup(now + Duration::from_secs(5)), None);
}
