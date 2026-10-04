"""journal.py against testdata/journal-sample.jsonl, which the journal's own test
(`the_sample_the_tools_read_is_what_the_journal_writes` in src/transitions/domain/journal.rs) pins to
what rini writes, and testdata/probe-sample.jsonl. Run `python3 -m unittest discover -s tools/focuslab`.
"""
import csv
import json
import os
import tempfile
import unittest

import journal

TESTDATA = os.path.join(os.path.dirname(os.path.abspath(__file__)), "testdata")
SAMPLE = os.path.join(TESTDATA, "journal-sample.jsonl")
PROBE = os.path.join(TESTDATA, "probe-sample.jsonl")


def sample():
    bursts = journal.read([SAMPLE])
    return bursts, journal.flights(bursts)


class Summary(unittest.TestCase):
    def setUp(self):
        self.bursts, self.flights = sample()
        self.clean, self.late = (flight.times() for flight in self.flights)

    def assertRow(self, row, expected):
        for key, value in expected.items():
            if value is None:
                self.assertIsNone(row[key], key)
            else:
                self.assertAlmostEqual(row[key], value, places=6, msg=key)

    def test_each_hold_is_a_flight_with_the_press_and_begin_pushed_before_it(self):
        self.assertEqual(len(self.bursts), 2)
        self.assertEqual([flight.g for flight in self.flights], [1, 2])
        self.assertEqual([flight.press["what"] for flight in self.flights], ["move_focus", "move_focus"])
        self.assertEqual([flight.marks[1]["mark"] for flight in self.flights], ["begin", "begin"])
        self.assertEqual(self.flights[0].marks[-1]["mark"], "report_settled", "a flight runs to its burst's end")
        self.assertEqual(self.flights[0].target, (1, 2))

    def test_press_to_show_splits_into_parts_in_order_that_sum_to_it(self):
        self.assertRow(self.clean, {
            "reactor": 2.4,
            "frame_zero_asked": 0.0,
            "service_wait": 1.3,
            "skylight_round": 29.7,
            "engine_wait": 2.2,
            "the_show": 0.4,
            "press_to_show": 36.0,
        })
        parts = sum(self.clean[key] for key, _ in journal.SPLIT if key != "press_to_show")
        self.assertAlmostEqual(parts, self.clean["press_to_show"], places=6)
        self.assertAlmostEqual(self.clean["press_to_show"], self.clean["overlay_up"], places=6)

    def test_the_rows_are_ms_after_the_press(self):
        self.assertRow(self.clean, {
            "compose": 6.1,
            "frame_zero_capture_starts": 3.7,
            "frame_zero_pictures_ready": 33.4,
            "overlay_up": 36.0,
            "rini_sees_it_landed": 90.8,
            "focused_picture_captured": 133.9,
            "show_to_landed": 70.4,
            "lift": 386.1,
            "lift_to_settled": 400.0,
        })

    def test_a_show_with_no_frame_zero_before_it_is_a_timeout_and_has_no_split(self):
        self.assertEqual(self.late["frame_zero_timeouts"], 1)
        self.assertEqual(self.clean["frame_zero_timeouts"], 0)
        self.assertRow(self.late, {key: None for key, _ in journal.SPLIT})
        self.assertRow(self.late, {
            "compose": 5.0,
            "frame_zero_capture_starts": None,
            "frame_zero_pictures_ready": None,
            "overlay_up": 81.8,
            "rini_sees_it_landed": 142.8,
            "focused_picture_captured": None,
            "lift": 851.8,
            "lift_to_settled": 400.0,
        })

    def test_show_to_landed_needs_every_end_window_landed_in_the_generation_that_lifted(self):
        self.assertIsNone(self.late["show_to_landed"], "window 1 was lost in the merged generation")

    def test_the_counts_per_flight(self):
        counts = lambda row: {key: row[key] for key, _ in journal.COUNTS}
        self.assertEqual(counts(self.clean), {
            "merges": 0, "swaps": 5, "snaps": 1, "stale_deliveries": 0, "lost": 0,
            "overdue_lifts": 0, "frame_zero_timeouts": 0,
        })
        self.assertEqual(counts(self.late), {
            "merges": 1, "swaps": 4, "snaps": 1, "stale_deliveries": 1, "lost": 1,
            "overdue_lifts": 1, "frame_zero_timeouts": 1,
        })

    def test_a_delivered_with_no_delivery_before_it_is_dropped(self):
        marks = self.flights[0].marks
        at = next(index for index, mark in enumerate(marks) if mark["mark"] == "delivery")
        rest = marks[at + 1:]
        found = journal.deliveries(rest)
        landed = next(index for index, mark in enumerate(rest) if mark["mark"] == "delivery")
        self.assertEqual(found[0][0], landed, "the frame-zero pictures before it are no delivery's")
        self.assertEqual([picture["window"] for picture in found[0][2]], [[1, 2]])

    def test_nearest_rank_percentiles(self):
        values = list(range(1, 11))
        self.assertEqual([journal.percentile(values, p) for p in (50, 90, 99)], [5, 9, 10])
        self.assertEqual(journal.percentile([7], 99), 7)

    def test_the_summary_prints_every_row_and_the_csv_has_one_line_a_flight(self):
        text = journal.summary(self.flights, self.bursts)
        for _, label in journal.SPLIT + journal.ROWS + journal.COUNTS:
            self.assertIn(label, text)
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "flights.csv")
            journal.write_csv(path, self.flights)
            with open(path) as file:
                rows = list(csv.DictReader(file))
        self.assertEqual([row["what"] for row in rows], ["move_focus", "move_focus"])
        self.assertEqual(rows[0]["press_to_show"], "36.0")
        self.assertEqual(rows[1]["press_to_show"], "")


class Handover(unittest.TestCase):
    def setUp(self):
        self.bursts, _ = sample()

    def test_each_window_is_named_by_the_kind_and_focus_of_the_generation_that_lifted(self):
        rows = journal.handovers(self.bursts)
        self.assertEqual([(row["of"], row["kind"], row["focused"]) for row in rows], [(1, "pan", True), (1, "pan", False)])
        self.assertEqual([(row["interior"], row["edge_in"], row["edge_out"]) for row in rows],
                         [(0.26, 0.37, 1.56), (0.79, 1.08, 2.26)])

    def test_each_held_lift_says_whether_it_hid_on_its_picture_and_how_long_it_held(self):
        self.assertEqual([(row["measured"], row["waited"]) for row in journal.holds(self.bursts)],
                         [(True, 31.4), (False, 60.0)])

    def test_a_held_lift_that_hid_unmeasured_has_no_handover(self):
        self.assertIn("1 of 2 held lifts measured, 2 windows", journal.handover(self.bursts))

    def test_the_headline_is_edge_in_p99_of_the_focused_window_on_pan_flights(self):
        text = journal.handover(self.bursts)
        self.assertIn("edge in p99, focused window, pan: 0.37 n=1", text)
        self.assertIn("pan, focused window", text)
        self.assertIn("pan, other window", text)
        for _, label in journal.HANDOVER:
            self.assertIn(label, text)


class Probe(unittest.TestCase):
    def setUp(self):
        _, found = sample()
        with open(PROBE) as file:
            self.joined = journal.join([json.loads(text) for text in file], found)

    def test_each_trial_joins_the_first_press_within_250ms_after_it_posted(self):
        self.assertEqual([(label, how) for label, how, _ in self.joined], [
            ("strip", "began"),
            ("overlay", "began"),
            ("strip", "another target"),
            ("strip", "joined"),
            ("strip", "unmatched"),
        ])
        self.assertAlmostEqual(self.joined[3][2]["post_to_press"], 1.2, places=6, msg="a press merged into a flight")

    def test_a_focuslab_trial_against_rinis_landing_and_focused_picture(self):
        row = self.joined[0][2]
        expected = {
            "post_to_press": 1.9,
            "post_to_show": 37.9,
            "post_to_landed": 92.7,
            "landed_after_still": 8.5,
            "post_to_look": 135.8,
            "look_after_look_full": 10.6,
            "post_to_lift": 388.0,
        }
        for key, value in expected.items():
            self.assertAlmostEqual(row[key], value, places=6, msg=key)
        self.assertIsNone(row["up_after_show"])

    def test_an_overlay_trial_against_rinis_show_and_lift(self):
        row = self.joined[1][2]
        for key, value in {"post_to_show": 83.9, "up_after_show": 6.5, "post_to_lift": 853.9, "gone_after_lift": 8.8}.items():
            self.assertAlmostEqual(row[key], value, places=6, msg=key)
        self.assertIsNone(row["look_after_look_full"])


if __name__ == "__main__":
    unittest.main()
