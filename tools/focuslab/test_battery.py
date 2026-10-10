"""The timing battery's reading: analyze.py over testdata/journal-sample.jsonl as a run's journal slice
and recordings built here, compare.py's rule, and battery.py's window lookup and ring switch. Run
`python3 -m unittest discover -s tools/focuslab`.
"""
import io
import json
import os
import shutil
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout

import analyze
import battery
import compare
import journal

TESTDATA = os.path.join(os.path.dirname(os.path.abspath(__file__)), "testdata")
SAMPLE = os.path.join(TESTDATA, "journal-sample.jsonl")
MS = 1_000_000
FIRST_PRESS, SECOND_PRESS, MERGED_PRESS = 81234557600000, 81237558200000, 81237708700000
BLURRED_PID = 1


def frame(t_ns, sig=100, vis=0.5, chroma=6.0):
    return {"type": "frame", "status": 0, "t_ns": t_ns, "vis": vis, "sig": sig, "pane": [50.0, 50.0, 50.0, chroma, 255.0]}


def run_dir(root, name="run", posts=None, frames=None, meta=None):
    """A battery run: the journal sample as its slice, one comms_pan recording, and its meta."""
    path = os.path.join(root, name)
    os.makedirs(path)
    shutil.copy(SAMPLE, os.path.join(path, "journal.slice.jsonl"))
    lines = [{"type": "window", "id": 1, "frame": [0, 32, 1728, 1085]}]
    lines += [{"type": "press", "code": code, "t_ns": t} for code, t in posts]
    lines += frames
    lines.sort(key=lambda line: line.get("t_ns", 0))
    with open(os.path.join(path, "rec-comms_pan.jsonl"), "w") as file:
        file.writelines(json.dumps(line) + "\n" for line in lines)
    meta = meta or {"label": name, "windows": {"comms": {"workspace": "comms", "app": "Code"},
                                              "spare": {"workspace": "spare", "app": "Messages"},
                                              "blur": {"workspace": "spare", "app": "Outlook", "pid": BLURRED_PID}}}
    with open(os.path.join(path, "meta.json"), "w") as file:
        json.dump(meta, file)
    return path


def sample_run(root, name="run", meta=None):
    """Four posts: one beginning the first flight, one the second, one the second's merged press, one
    nothing. The first is seen up 70ms after its post, paced 16, 16, 40 and 17ms with one repeated
    picture, and gone at an opaque all-black frame; the second shows three frames of a flat pane."""
    first, second = FIRST_PRESS - 5 * MS, SECOND_PRESS - 10 * MS
    posts = [(38, first), (37, second), (38, MERGED_PRESS - 1 * MS), (37, 81239000000000)]
    up = first + 70 * MS
    frames = [frame(first + 10 * MS, sig=0, vis=0.0),
              frame(up, sig=1, chroma=7.0), frame(up + 16 * MS, sig=2, chroma=6.0),
              frame(up + 32 * MS, sig=2, chroma=5.0), frame(up + 72 * MS, sig=3, chroma=3.0),
              frame(up + 89 * MS, sig=4, chroma=6.0), frame(up + 300 * MS, sig=0),
              frame(up + 317 * MS, sig=0, vis=0.0)]
    frames += [frame(second + 80 * MS + n * 17 * MS, sig=10 + n, chroma=1.0) for n in range(3)]
    return run_dir(root, name, posts, frames, meta)


class Analyze(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp()
        self.run = sample_run(self.root)
        with redirect_stdout(io.StringIO()):
            analyze.main(self.run)
        with open(os.path.join(self.run, "metrics.json")) as file:
            self.metrics = json.load(file)
        self.rows = self.metrics["flights"]

    def tearDown(self):
        shutil.rmtree(self.root)

    def test_each_post_joins_the_flight_whose_press_follows_it_within_250ms(self):
        self.assertEqual([row["join"] for row in self.rows], ["began", "began", "joined", "unmatched"])
        self.assertEqual([row.get("g") for row in self.rows], [1, 2, None, None])
        self.assertEqual([row["key"] for row in self.rows], ["alt+j", "alt+l", "alt+j", "alt+l"])
        self.assertAlmostEqual(self.rows[0]["post_to_press"], 5.0)
        self.assertAlmostEqual(self.rows[1]["post_to_press"], 10.0)

    def test_a_joined_flight_carries_the_journals_times(self):
        found = journal.flights(journal.read([SAMPLE]))
        for row, flight in zip(self.rows, found):
            t = flight.times()
            self.assertEqual((row["overlay_up"], row["lift"], row["kind"]), (t["overlay_up"], t["lift"], flight.kind()))
        self.assertEqual(self.metrics["by_kind"]["pan"]["flights"], 2)

    def test_pacing_counts_the_gaps_between_the_frames_shown_until_the_overlay_goes(self):
        row = self.rows[0]
        self.assertEqual(row["frames"], 5, "the black frame is not shown: it ends the flight")
        self.assertEqual(row["intervals"], {"n": 4, "p50": 16.0, "p90": 40.0, "p99": 40.0, "max": 40.0})
        self.assertEqual((row["over_20ms"], row["over_25ms"], row["over_33ms"]), (1, 1, 1))
        self.assertEqual(row["repeats"], 1)
        self.assertEqual(row["black_frames"], 1)
        self.assertAlmostEqual(row["screen_up"], 70.0)
        self.assertAlmostEqual(row["screen_gone"], 370.0)
        self.assertEqual(self.rows[3]["frames"], 0)
        self.assertEqual(self.metrics["segments"]["comms_pan"]["flights_without_frames"], 2)
        self.assertEqual(self.metrics["pacing_all"]["intervals_n"], 6)

    def test_the_blur_check_reads_the_pane_over_the_last_three_frames_of_a_fly_in(self):
        self.assertEqual(self.rows[0]["blur"]["per_frame_chroma"], [5.0, 3.0, 6.0])
        self.assertEqual(self.rows[0]["blur"]["chroma"], 4.67)
        self.assertFalse(self.rows[0]["blur"]["pass"])
        self.assertEqual(self.metrics["blur"]["flights"], 2)
        self.assertEqual(self.metrics["blur"]["passed"], 0)

    def test_a_fly_in_passes_at_a_chroma_of_five(self):
        shown = [frame(n, chroma=c) for n, c in enumerate((1.0, 4.0, 5.0, 6.0))]
        self.assertTrue(analyze.blur(shown)["pass"])
        self.assertFalse(analyze.blur(shown[:-1])["pass"])

    def test_only_fly_ins_to_the_blurred_window_are_checked(self):
        meta = {"label": "other", "windows": {"blur": {"app": "Outlook", "pid": 99}}}
        run = sample_run(self.root, "other", meta)
        with redirect_stdout(io.StringIO()):
            analyze.main(run)
        with open(os.path.join(run, "metrics.json")) as file:
            self.assertEqual(json.load(file)["blur"]["flights"], 0)

    def test_every_drain_the_slice_journalled_is_split_by_show_flight_and_lift(self):
        phases = self.metrics["drains_by_phase"]
        self.assertEqual(phases["show"]["queued"], {"n": 2, "p50": 0.3, "p90": 0.4, "p99": 0.4, "max": 0.4})
        self.assertEqual(phases["flight"]["applied"], {"n": 2, "p50": 0.4, "p90": 1.3, "p99": 1.3, "max": 1.3})
        self.assertEqual(phases["lift"]["applied"]["max"], 0.4)

    def test_the_summary_names_the_windows_the_blur_check_flew_between(self):
        with open(os.path.join(self.run, "summary.txt")) as file:
            text = file.read()
        self.assertIn("blur, Messages->Outlook", text)
        self.assertIn("0/2 pass", text)
        self.assertIn("presses not beginning a flight: comms_pan#2 alt+j (joined), comms_pan#3 alt+l (unmatched)", text)

    def test_the_crop_drawn_tiles_of_the_joined_flights_are_counted_with_their_ground_reads(self):
        self.assertEqual(self.metrics["crops"], {"drains": 1, "fills": 1, "ground": {
            "n": 1, "p50": 7.91, "p90": 7.91, "p99": 7.91, "max": 7.91}})
        self.assertEqual(self.metrics["drains_all"]["ground"]["max"], 7.91)
        with open(os.path.join(self.run, "summary.txt")) as file:
            self.assertIn("crop-grid fills 1 in 1 drains, their grounds read 7.9/7.9/7.9 n=1", file.read())

    def test_compare_reads_87_metrics_from_a_run(self):
        read = compare.metrics(self.run)
        self.assertEqual(len(read), 87)
        self.assertEqual(read["phase.show.queued.max"], 0.4)
        self.assertEqual(read["pacing.comms_pan.over_33ms"], 1)
        self.assertIsNone(read["pacing.switch.interval.p50"], "no switch was recorded")
        self.assertIsNone(read["switch.lift.p50"], "no switch was flown")
        self.assertEqual(read["blur.failed"], 2)


class Rule(unittest.TestCase):
    def verdict(self, base, cand, base2=None, cand2=None, name="pan.lift.p50"):
        return compare.judge(name, base, cand, base2, cand2)["verdict"]

    def test_a_metric_passes_with_the_candidates_median_at_most_the_bases_max(self):
        self.assertEqual(self.verdict([1, 2, 3], [3, 3, 9]), "pass")
        self.assertEqual(self.verdict([1, 2, 3], [0, 0, 0]), "pass", "below the spread is not a regression")
        self.assertEqual(self.verdict([1, 2, 3], [4, 4, 1]), "needs stage 2")

    def test_stage_two_judges_six_against_six_by_the_same_rule(self):
        self.assertEqual(self.verdict([1, 2, 3], [4, 4, 1], [5, 1, 1], [1, 1, 1]), "pass")
        self.assertEqual(self.verdict([1, 2, 3], [4, 4, 1], [1, 1, 1], [9, 9, 9]), "regression")
        row = compare.judge("pan.lift.p50", [1, 2, 3], [4, 4, 1], [1, 1, 1], [9, 9, 9])
        self.assertEqual((row["stage1"], row["stage2_base_max"], row["stage2_cand_median"]), ("FAIL", 3, 6.5))

    def test_a_metric_passing_stage_one_is_not_judged_again(self):
        row = compare.judge("pan.lift.p50", [1, 2, 3], [3, 3, 3], [1, 1, 1], [9, 9, 9])
        self.assertEqual((row["verdict"], row["stage2_base_max"]), ("pass", ""))

    def test_blur_passes_only_with_no_failed_fly_in_in_any_run(self):
        blur = lambda *sides: self.verdict(*sides, name=compare.BLUR)
        self.assertEqual(blur([0, 0, 0], [0, 0, 0]), "pass")
        self.assertEqual(blur([0, 0, 0], [0, 1, 0]), "regression", "the median is 0, and still it fails")
        self.assertEqual(blur([1, 0, 0], [0, 0, 0]), "regression", "the base failing fails it too")
        self.assertEqual(blur([0, 0, 0], [0, 1, 0], [0, 0, 0], [0, 0, 0]), "regression", "stage 2 cannot undo one")
        self.assertEqual(blur([0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 1]), "regression")

    def test_a_metric_some_run_has_no_number_for_is_missing(self):
        self.assertEqual(self.verdict([1, None, 3], [1, 1, 1]), "missing")
        self.assertEqual(self.verdict([1, 2, 3], [4, 4, 4], [1, 1, None], [1, 1, 1]), "missing")
        self.assertEqual(self.verdict([0, 0, 0], [None, 0, 0], name=compare.BLUR), "missing",
                         "a run that flew into no blurred window checked nothing")

    def test_the_verdict_accepts_only_when_every_metric_passes(self):
        rows = [compare.judge("a", [1], [1]), compare.judge("b", [1], [2]), compare.judge("c", [None], [1])]
        accepted, text = compare.verdict(rows)
        self.assertFalse(accepted)
        self.assertTrue(text.startswith("3 metrics: 1 pass, 1 need stage 2, 0 regressions, 1 missing: NOT ACCEPTED"))
        self.assertIn("needs stage 2: b", text)
        self.assertTrue(compare.verdict(rows[:1])[0])

    def test_compare_runs_over_run_directories_and_exits_by_the_verdict(self):
        root = tempfile.mkdtemp()
        try:
            runs = []
            for n in range(2):
                runs.append(sample_run(root, f"r{n}"))
                with redirect_stdout(io.StringIO()):
                    analyze.main(runs[-1])
            out, err = io.StringIO(), io.StringIO()
            with redirect_stdout(out), redirect_stderr(err):
                code = compare.main([runs[0], "--", runs[1]])
            lines = out.getvalue().splitlines()
            self.assertEqual(lines[0], ",".join(compare.COLUMNS))
            self.assertEqual(len(lines), 88)
            self.assertEqual(code, 1, "the switch metrics are missing and blur failed")
            self.assertIn("blur.failed", err.getvalue())
            with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                self.assertEqual(compare.main([runs[0], runs[1]]), 2)
                self.assertEqual(compare.main([runs[0], "--", runs[1], "--stage2", runs[0]]), 2)
        finally:
            shutil.rmtree(root)


    def test_runs_flying_another_scene_are_refused_before_any_metric_is_judged(self):
        root = tempfile.mkdtemp()
        try:
            runs = []
            for n in range(2):
                runs.append(sample_run(root, f"r{n}"))
                with redirect_stdout(io.StringIO()):
                    analyze.main(runs[-1])
            slice_path = os.path.join(runs[1], "journal.slice.jsonl")
            with open(slice_path) as file:
                text = file.read()
            with open(slice_path, "w") as file:
                file.write(text.replace('"frame":[866.0,35.0,860.0,1079.0]', '"frame":[3.0,724.0,1148.0,390.0]'))
            out, err = io.StringIO(), io.StringIO()
            with redirect_stdout(out), redirect_stderr(err):
                self.assertEqual(compare.main([runs[0], "--", runs[1]]), 3)
            self.assertEqual(out.getvalue(), "", "no metric judged")
            self.assertIn(f"{runs[1]} flew another scene than {runs[0]}", err.getvalue())
            self.assertIn("delivered at [3.0, 724.0, 1148.0, 390.0], which it never was", err.getvalue())
            self.assertIn("never delivered at [866.0, 35.0, 860.0, 1079.0]", err.getvalue())
        finally:
            shutil.rmtree(root)

    def test_a_scene_is_its_segments_kinds_in_order_and_its_frames_delivered_within_a_point(self):
        a = {"kinds": {"comms_pan": ["bounce", "pan"]}, "frames": [[577.0, 35.0, 1148.0, 1079.0], [866.0, 35.0, 860.0, 1079.0]]}
        nudged = {"kinds": a["kinds"], "frames": [[577.0, 35.0, 1148.0, 1079.0], [865.0, 35.0, 860.0, 1079.0]]}
        self.assertEqual(compare.scene_differences(a, nudged), [], "a point off is the same window")
        panned = dict(a, kinds={"comms_pan": ["pan", "pan"]})
        self.assertEqual(compare.scene_differences(a, panned),
                         ["comms_pan flew ['pan', 'pan'], not ['bounce', 'pan']"], "press 0 panned")
        self.assertEqual(compare.scene_differences(a, dict(a, kinds={})), ["comms_pan flew None, not ['bounce', 'pan']"])

    def test_a_run_wearing_a_picture_over_five_seconds_old_is_flagged_and_still_judged(self):
        root = tempfile.mkdtemp()
        try:
            runs = []
            for n in range(2):
                runs.append(sample_run(root, f"r{n}"))
                with redirect_stdout(io.StringIO()):
                    analyze.main(runs[-1])
            stale = {"t": 81234566000000, "g": 1, "mark": "wears", "window": [1, 1], "at": "compose",
                     "route": "skylight", "blur": True, "taken_before_us": 744_000_000}
            with open(os.path.join(runs[1], "journal.slice.jsonl"), "a") as file:
                file.write(json.dumps(stale) + "\n")
            self.assertEqual(compare.stale_wears(runs[0]), (0, None), "the sample's oldest is 2.1s")
            self.assertEqual(compare.stale_wears(runs[1]), (1, (744_000_000, [1, 1])))
            err = io.StringIO()
            with redirect_stdout(io.StringIO()), redirect_stderr(err):
                self.assertEqual(compare.main([runs[0], "--", runs[1]]), 1, "judged as before")
            self.assertIn(f"flag: {runs[1]}: 1 pictures worn over 5s old, the oldest 744.0s on [1, 1]", err.getvalue())
            self.assertNotIn(f"flag: {runs[0]}", err.getvalue())
        finally:
            shutil.rmtree(root)


WORKSPACES = [
    {"name": "main", "is_active": False, "windows": [
        {"id": {"pid": 5, "idx": 9}, "app_name": "Code", "is_focused": False}]},
    {"name": "comms", "is_active": True, "windows": [
        {"id": {"pid": 2, "idx": 53}, "app_name": "Ghostty", "is_focused": True},
        {"id": {"pid": 5, "idx": 245}, "app_name": "Code", "is_focused": False},
        {"id": {"pid": 5, "idx": 246}, "app_name": "Code", "is_focused": False}]},
    {"name": "spare", "is_active": False, "windows": [
        {"id": {"pid": 7, "idx": 73}, "app_name": "Messages", "is_focused": True},
        {"id": {"pid": 8, "idx": 67}, "app_name": None, "is_focused": False}]},
]
def placed(pid, idx, app, x, y, w, h, floating=False):
    return {"id": {"pid": pid, "idx": idx}, "app_name": app, "is_focused": False, "is_floating": floating,
            "frame": {"origin": {"x": x, "y": y}, "size": {"width": w, "height": h}}}


COMMS = [{"name": "comms", "is_active": True, "windows": [
    placed(48655, 33135, "Code", 3, 35, 568, 1079),
    placed(3086, 53, "Ghostty", 577, 35, 1148, 1079),
    placed(3086, 16842, "Ghostty", 1731, 35, 860, 1079),
    placed(9, 1, "Calculator", 200, 200, 230, 400, floating=True)]}]
MIRRORED = [{"name": "comms", "is_active": True, "windows": [
    placed(3086, 2534, "Ghostty", 3, 35, 1148, 686),
    placed(3086, 53, "Ghostty", 3, 724, 1148, 390),
    placed(48655, 33135, "Code", 1157, 35, 568, 1079)]}]
CONFIG = """layout {
    gaps 8
    focus-ring {
        active-color "#ff7c50"
    }
}

window-rule {
    match app-id="Zoom"
    focus-ring {
        width 4
    }
}
"""


class Setup(unittest.TestCase):
    def test_a_window_is_the_first_of_its_app_on_the_named_workspace(self):
        self.assertEqual(battery.window(WORKSPACES, "comms", "Code"), {"pid": 5, "idx": 245})
        self.assertEqual(battery.window(WORKSPACES, "main", "Code"), {"pid": 5, "idx": 9})
        self.assertIsNone(battery.window(WORKSPACES, "spare", "Code"))
        self.assertIsNone(battery.window(WORKSPACES, "nowhere", "Code"))

    def test_the_focused_window_is_the_active_workspaces(self):
        self.assertEqual(battery.focused(WORKSPACES), ({"pid": 2, "idx": 53}, "comms", "Ghostty"))
        self.assertIsNone(battery.focused([dict(WORKSPACES[2], is_active=True, windows=[])]))

    def test_a_scene_is_the_workspaces_windows_by_app_and_frame_and_where_its_window_is(self):
        found = battery.scene(COMMS, "comms", "Code")
        self.assertEqual(found["focus"], [3, 35, 568, 1079])
        self.assertEqual([(w["app"], w["frame"][0], w["floating"]) for w in found["windows"]],
                         [("Code", 3, False), ("Ghostty", 577, False), ("Ghostty", 1731, False), ("Calculator", 200, True)])
        self.assertEqual(battery.scene_problems(found), [])
        self.assertIsNone(battery.scene(COMMS, "spare", "Code"))
        self.assertEqual(battery.scene_problems(None), ["no such workspace"])
        self.assertEqual(battery.scene_problems(battery.scene(COMMS, "comms", "Zoom")), ["no Zoom window on comms"])

    def test_a_scene_whose_window_is_not_the_leftmost_column_is_refused_since_its_first_press_would_pan(self):
        problems = battery.scene_problems(battery.scene(MIRRORED, "comms", "Code"))
        self.assertEqual(problems, ["Code is not the leftmost column of comms (x 1157 against 3): "
                                    "its first press would pan, not bounce"])

    def test_a_scene_is_refused_unless_every_window_is_where_its_reference_had_it(self):
        reference = battery.scene(COMMS, "comms", "Code")
        self.assertEqual(battery.scene_problems(reference, reference), [])
        nudged = json.loads(json.dumps(COMMS))
        nudged[0]["windows"][1]["frame"]["origin"]["x"] = 578
        self.assertEqual(battery.scene_problems(battery.scene(nudged, "comms", "Code"), reference), [], "a point off")
        stacked = json.loads(json.dumps(COMMS))
        stacked[0]["windows"][1]["frame"]["size"]["height"] = 686
        self.assertEqual(battery.scene_problems(battery.scene(stacked, "comms", "Code"), reference),
                         ["Ghostty at [577, 35, 1148, 686] is not in the reference",
                          "Ghostty at [577, 35, 1148, 1079] is missing"])
        meta = {"label": "old-1", "scene": {"comms_pan": reference}}
        self.assertEqual(battery.reference_scene(meta, "comms"), reference, "a run's meta.json holds its scenes")
        self.assertIsNone(battery.reference_scene(meta, "spare"))

    def test_a_parked_window_is_matched_by_its_size_wherever_macos_clamped_its_corner(self):
        parked = json.loads(json.dumps(COMMS))
        parked[0]["windows"][1]["frame"]["origin"] = {"x": 1727, "y": 1085}
        reference = battery.scene(parked, "comms", "Code")
        moved = json.loads(json.dumps(parked))
        moved[0]["windows"][1]["frame"]["origin"] = {"x": 1727, "y": 1116}
        self.assertEqual(battery.scene_problems(battery.scene(moved, "comms", "Code"), reference), [])
        resized = json.loads(json.dumps(moved))
        resized[0]["windows"][1]["frame"]["size"]["width"] = 860
        self.assertEqual(battery.scene_problems(battery.scene(resized, "comms", "Code"), reference),
                         ["Ghostty at [1727, 1116, 860, 1079] is not in the reference",
                          "Ghostty at [1727, 1085, 1148, 1079] is missing"])

    def test_the_scene_command_prints_the_scene_and_exits_one_naming_why_it_is_refused(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as file:
            json.dump({"scene": {"comms_pan": battery.scene(COMMS, "comms", "Code")}}, file)
        try:
            for workspaces, args, code in ((COMMS, [], 0), (COMMS, [file.name], 0), (MIRRORED, [], 1), (MIRRORED, [file.name], 1)):
                out, err = io.StringIO(), io.StringIO()
                stdin, sys.stdin = sys.stdin, io.StringIO(json.dumps(workspaces))
                try:
                    with redirect_stdout(out), redirect_stderr(err):
                        self.assertEqual(battery.main(["scene", "comms", "Code", *args]), code, args)
                finally:
                    sys.stdin = stdin
                self.assertEqual(json.loads(out.getvalue())["workspace"], "comms")
                self.assertEqual(bool(err.getvalue()), bool(code))
            stdin, sys.stdin = sys.stdin, io.StringIO(json.dumps(COMMS))
            try:
                with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                    self.assertEqual(battery.main(["scene", "spare", "Messages", file.name]), 1, "no scene of spare")
            finally:
                sys.stdin = stdin
        finally:
            os.unlink(file.name)

    def test_the_ring_turns_off_and_on_in_the_layouts_block_alone(self):
        off = battery.set_ring(CONFIG, "off")
        self.assertEqual(off, CONFIG.replace("    focus-ring {\n", "    focus-ring {\n        off\n", 1))
        self.assertEqual(battery.ring_state(off), "off")
        self.assertEqual(battery.set_ring(off, "off"), off)
        self.assertEqual(battery.set_ring(off, "on"), CONFIG)
        self.assertEqual(battery.ring_state(CONFIG), "on")

    def test_a_config_without_exactly_one_such_block_is_left_alone(self):
        for text in ("layout {\n    gaps 8\n}\n", "layout {\n    focus-ring { off; }\n}\n",
                     CONFIG.replace("}\n\nwindow", "    focus-ring {\n    }\n}\n\nwindow", 1)):
            with self.assertRaises(battery.NoRing):
                battery.set_ring(text, "off")
        with tempfile.NamedTemporaryFile("w", suffix=".kdl", delete=False) as file:
            file.write("layout {\n}\n")
        try:
            with redirect_stderr(io.StringIO()):
                self.assertEqual(battery.main(["ring", "off", file.name]), 2)
            with open(file.name) as kept:
                self.assertEqual(kept.read(), "layout {\n}\n")
        finally:
            os.unlink(file.name)


if __name__ == "__main__":
    unittest.main()
