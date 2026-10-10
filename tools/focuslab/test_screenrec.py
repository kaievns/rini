"""The lab's reading: screenrec.py's workspace and window lookups, where a press's frames sit in a
screenrec movie, and how many of a frame's blocks are flat and dark. Run
`python3 -m unittest discover -s tools/focuslab`.
"""
import unittest

import screenrec

WORKSPACES = [
    {"name": "comms", "index": 0, "is_active": True, "windows": [
        {"id": {"pid": 2, "idx": 53}, "title": "lab notes", "frame": [3, 35, 860, 1079]}]},
    {"name": "work", "index": 2, "is_active": False, "windows": [
        {"id": {"pid": 7, "idx": 1}, "title": "lab-C", "frame": [3, 35, 860, 537]},
        {"id": {"pid": 9, "idx": 4}, "title": None, "frame": [866, 35, 860, 1079]},
        {"id": {"pid": 8, "idx": 1}, "title": "lab-D", "frame": [3, 578, 860, 536]}]},
]

RUN = {"scale": 2, "presses": [{"code": 43, "mods": 524288, "ms": 644.5}, {"code": 3, "mods": 524288, "ms": 10.0}]}


class Lab(unittest.TestCase):
    def test_a_workspace_is_switched_to_by_its_index(self):
        self.assertEqual(screenrec.index(WORKSPACES, "work"), 2)
        self.assertEqual(screenrec.index(WORKSPACES, "comms"), 0)
        self.assertIsNone(screenrec.index(WORKSPACES, "nowhere"))

    def test_the_lab_windows_are_those_titled_lab_on_the_named_workspace(self):
        found = screenrec.lab(WORKSPACES, "work")
        self.assertEqual(found, [({"pid": 7, "idx": 1}, "lab-C", [3, 35, 860, 537]),
                                 ({"pid": 8, "idx": 1}, "lab-D", [3, 578, 860, 536])])
        self.assertEqual(screenrec.lab(WORKSPACES, "comms"), [])


class Cut(unittest.TestCase):
    def test_a_press_is_cut_from_20ms_before_it(self):
        self.assertEqual(screenrec.cut(RUN, 1, 700), (0.6245, 0.7, "scale=576:-1"))

    def test_a_press_in_the_first_20ms_is_cut_from_the_start(self):
        self.assertEqual(screenrec.cut(RUN, 2, 700)[0], 0.0)

    def test_a_crop_in_points_is_read_at_the_movies_scale(self):
        self.assertEqual(screenrec.cut(RUN, 1, 700, "3,35,860,537", 288)[2], "crop=1720:1074:6:70,scale=288:-1")


def frame(width, height, at=lambda x, y: 200):
    return bytes(at(x, y) for y in range(height) for x in range(width))


class Blank(unittest.TestCase):
    def test_a_flat_dark_block_counts(self):
        dark = frame(16, 32, lambda x, y: 20 if x < 8 and 16 <= y < 24 else 200)
        self.assertEqual(screenrec.blank_blocks(dark, 16, 32), 1)

    def test_a_flat_block_as_bright_as_the_dark_limit_does_not(self):
        self.assertEqual(screenrec.blank_blocks(frame(16, 32, lambda x, y: 59), 16, 32), 4)
        self.assertEqual(screenrec.blank_blocks(frame(16, 32, lambda x, y: 60), 16, 32), 0)

    def test_a_dark_block_with_texture_does_not(self):
        textured = frame(8, 24, lambda x, y: 10 + 6 * ((x + y) % 2))
        self.assertEqual(screenrec.blank_blocks(textured, 8, 24), 0)
        faint = frame(8, 24, lambda x, y: 10 + 4 * ((x + y) % 2))
        self.assertEqual(screenrec.blank_blocks(faint, 8, 24), 1)

    def test_the_bar_rows_are_skipped(self):
        self.assertEqual(screenrec.blank_blocks(frame(8, 16, lambda x, y: 0), 8, 16), 0)
        self.assertEqual(screenrec.blank_blocks(frame(8, 24, lambda x, y: 0), 8, 24), 1)

    def test_a_part_block_at_the_edges_is_left_out(self):
        self.assertEqual(screenrec.blank_blocks(frame(15, 31, lambda x, y: 0), 15, 31), 1)


if __name__ == "__main__":
    unittest.main()
