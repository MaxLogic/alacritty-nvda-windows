//! UIA event throttling tests.

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::UI::Accessibility::{
        UIA_ActiveTextPositionChangedEventId, UIA_NotificationEventId, UIA_Text_TextChangedEventId,
        UIA_Text_TextSelectionChangedEventId,
    };

    use crate::accessibility::windows_provider::{UiaEventSink, UiaEventThrottle};

    #[derive(Default)]
    struct RecordingSink {
        listening: bool,
        events: Vec<i32>,
        active_text_position_events: usize,
        notifications: Vec<String>,
    }

    impl UiaEventSink for RecordingSink {
        fn clients_are_listening(&self) -> bool {
            self.listening
        }

        fn raise_event(&mut self, _provider: *mut c_void, event_id: i32) {
            self.events.push(event_id);
        }

        fn raise_active_text_position_changed(&mut self, _provider: *mut c_void) {
            self.active_text_position_events += 1;
        }

        fn raise_notification(&mut self, _provider: *mut c_void, text: &str) {
            self.events.push(UIA_NotificationEventId);
            self.notifications.push(text.to_owned());
        }
    }

    #[test]
    fn skips_events_when_no_uia_clients_are_listening() {
        let start = Instant::now();
        let mut throttle = UiaEventThrottle::new(Duration::from_millis(75), start);
        let mut sink = RecordingSink {
            listening: false,
            events: Vec::new(),
            active_text_position_events: 0,
            notifications: Vec::new(),
        };

        throttle.record_snapshot_change(true, true, true, Some("output".to_owned()));
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(100), &mut sink);

        assert!(sink.events.is_empty());
        assert_eq!(sink.active_text_position_events, 0);
        assert!(sink.notifications.is_empty());
    }

    #[test]
    fn emits_first_change_immediately_then_coalesces_to_one_batch_per_interval() {
        let start = Instant::now();
        let mut throttle = UiaEventThrottle::new(Duration::from_millis(75), start);
        let mut sink = RecordingSink {
            listening: true,
            events: Vec::new(),
            active_text_position_events: 0,
            notifications: Vec::new(),
        };

        throttle.record_snapshot_change(true, true, true, Some("first".to_owned()));
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(30), &mut sink);
        assert_eq!(
            sink.events,
            [
                UIA_Text_TextChangedEventId,
                UIA_Text_TextSelectionChangedEventId,
                UIA_NotificationEventId
            ]
        );
        assert_eq!(sink.active_text_position_events, 1);
        assert_eq!(sink.notifications, ["first"]);

        throttle.record_snapshot_change(true, true, true, Some("second".to_owned()));
        throttle.record_snapshot_change(true, true, true, Some("third".to_owned()));
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(60), &mut sink);
        assert_eq!(
            sink.events,
            [
                UIA_Text_TextChangedEventId,
                UIA_Text_TextSelectionChangedEventId,
                UIA_NotificationEventId,
                UIA_NotificationEventId,
            ]
        );
        assert_eq!(sink.active_text_position_events, 2);
        assert_eq!(sink.notifications, ["first", "second\nthird"]);

        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(105), &mut sink);
        assert_eq!(
            sink.events,
            [
                UIA_Text_TextChangedEventId,
                UIA_Text_TextSelectionChangedEventId,
                UIA_NotificationEventId,
                UIA_NotificationEventId,
                UIA_Text_TextChangedEventId,
                UIA_Text_TextSelectionChangedEventId,
            ]
        );
        assert_eq!(sink.active_text_position_events, 2);
        assert_eq!(sink.notifications, ["first", "second\nthird"]);
    }

    #[test]
    fn emits_latest_pending_change_after_previous_batch() {
        let start = Instant::now();
        let mut throttle = UiaEventThrottle::new(Duration::from_millis(75), start);
        let mut sink = RecordingSink {
            listening: true,
            events: Vec::new(),
            active_text_position_events: 0,
            notifications: Vec::new(),
        };

        throttle.record_snapshot_change(true, false, false, None);
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(75), &mut sink);
        throttle.record_snapshot_change(false, true, true, None);
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(150), &mut sink);

        assert_eq!(
            sink.events,
            [UIA_Text_TextChangedEventId, UIA_Text_TextSelectionChangedEventId]
        );
        assert_eq!(sink.active_text_position_events, 1);
    }

    #[test]
    fn emits_caret_only_change_immediately_inside_throttle_interval() {
        let start = Instant::now();
        let mut throttle = UiaEventThrottle::new(Duration::from_millis(75), start);
        let mut sink = RecordingSink {
            listening: true,
            events: Vec::new(),
            active_text_position_events: 0,
            notifications: Vec::new(),
        };

        throttle.record_snapshot_change(true, true, true, None);
        throttle.flush_due(std::ptr::null_mut(), start, &mut sink);
        throttle.record_snapshot_change(false, true, true, None);
        throttle.flush_due(std::ptr::null_mut(), start + Duration::from_millis(10), &mut sink);

        assert_eq!(sink.active_text_position_events, 2);
    }

    #[test]
    fn emits_text_pattern2_active_text_position_through_dedicated_callback() {
        let start = Instant::now();
        let mut throttle = UiaEventThrottle::new(Duration::from_millis(75), start);
        let mut sink = RecordingSink {
            listening: true,
            events: Vec::new(),
            active_text_position_events: 0,
            notifications: Vec::new(),
        };

        throttle.record_snapshot_change(false, false, true, None);
        throttle.flush_due(std::ptr::null_mut(), start, &mut sink);

        assert!(sink.events.is_empty());
        assert!(!sink.events.contains(&UIA_ActiveTextPositionChangedEventId));
        assert_eq!(sink.active_text_position_events, 1);
    }
}
