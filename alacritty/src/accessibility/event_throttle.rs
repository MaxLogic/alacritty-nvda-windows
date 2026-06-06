//! UIA event throttling tests.

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::UI::Accessibility::{
        UIA_Text_TextChangedEventId, UIA_Text_TextSelectionChangedEventId,
    };

    use crate::accessibility::windows_provider::{UiaEventSink, UiaEventThrottle};

    #[derive(Default)]
    struct RecordingSink {
        listening: bool,
        events: Vec<i32>,
    }

    impl UiaEventSink for RecordingSink {
        fn clients_are_listening(&self) -> bool {
            self.listening
        }

        fn raise_event(&mut self, _provider: *mut c_void, event_id: i32) {
            self.events.push(event_id);
        }
    }

    #[test]
    fn skips_events_when_no_uia_clients_are_listening() {
        let start = Instant::now();
        let mut throttle = UiaEventThrottle::new(Duration::from_millis(75), start);
        let mut sink = RecordingSink { listening: false, events: Vec::new() };

        throttle.record_snapshot_change(true, true);
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(100), &mut sink);

        assert!(sink.events.is_empty());
    }

    #[test]
    fn emits_first_change_immediately_then_coalesces_to_one_batch_per_interval() {
        let start = Instant::now();
        let mut throttle = UiaEventThrottle::new(Duration::from_millis(75), start);
        let mut sink = RecordingSink { listening: true, events: Vec::new() };

        throttle.record_snapshot_change(true, true);
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(30), &mut sink);
        assert_eq!(sink.events, [
            UIA_Text_TextChangedEventId,
            UIA_Text_TextSelectionChangedEventId
        ]);

        throttle.record_snapshot_change(true, true);
        throttle.record_snapshot_change(true, true);
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(60), &mut sink);
        assert_eq!(sink.events.len(), 2);

        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(105), &mut sink);
        assert_eq!(sink.events, [
            UIA_Text_TextChangedEventId,
            UIA_Text_TextSelectionChangedEventId,
            UIA_Text_TextChangedEventId,
            UIA_Text_TextSelectionChangedEventId,
        ]);
    }

    #[test]
    fn emits_latest_pending_change_after_previous_batch() {
        let start = Instant::now();
        let mut throttle = UiaEventThrottle::new(Duration::from_millis(75), start);
        let mut sink = RecordingSink { listening: true, events: Vec::new() };

        throttle.record_snapshot_change(true, false);
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(75), &mut sink);
        throttle.record_snapshot_change(false, true);
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(150), &mut sink);

        assert_eq!(sink.events, [
            UIA_Text_TextChangedEventId,
            UIA_Text_TextSelectionChangedEventId
        ]);
    }

    #[test]
    fn does_not_emit_text_pattern2_active_text_position_event() {
        let start = Instant::now();
        let mut throttle = UiaEventThrottle::new(Duration::from_millis(75), start);
        let mut sink = RecordingSink { listening: true, events: Vec::new() };

        throttle.record_snapshot_change(false, true);
        throttle.flush_due(std::ptr::null_mut(), start, &mut sink);

        assert_eq!(sink.events, [UIA_Text_TextSelectionChangedEventId]);
    }
}
