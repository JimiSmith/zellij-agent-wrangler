"""Unit tests for the terminal emulator in screen.py.

Each case is a byte string with a known result, so the emulator can be trusted
before it is pointed at anything as noisy as zellij. Run with:

    python3 -m unittest discover -s tests -v
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from screen import Screen, Sgr  # noqa: E402


def replay(data, rows=5, cols=20):
    screen = Screen(rows, cols)
    screen.feed(data)
    return screen


class TestPlainText(unittest.TestCase):
    def test_writes_and_wraps(self):
        screen = replay(b"hello", rows=2, cols=10)
        self.assertEqual(screen.line(0), "hello")
        self.assertEqual(screen.text(), ["hello", ""])

    def test_carriage_return_overwrites(self):
        screen = replay(b"hello\rHE")
        self.assertEqual(screen.line(0), "HEllo")

    def test_newline_moves_down_without_returning(self):
        screen = replay(b"ab\ncd")
        self.assertEqual(screen.line(0), "ab")
        self.assertEqual(screen.line(1), "  cd")

    def test_backspace_moves_left_without_erasing(self):
        screen = replay(b"abc\b\bX")
        self.assertEqual(screen.line(0), "aXc")

    def test_wrap_to_next_row(self):
        screen = replay(b"abcdef", rows=3, cols=4)
        self.assertEqual(screen.line(0), "abcd")
        self.assertEqual(screen.line(1), "ef")

    def test_scrolls_at_the_bottom(self):
        screen = replay(b"one\r\ntwo\r\nthree", rows=2, cols=10)
        self.assertEqual(screen.text(), ["two", "three"])


class TestCursorAddressing(unittest.TestCase):
    def test_absolute_position(self):
        screen = replay(b"\x1b[3;5Hx")
        self.assertEqual(screen.find("x"), (2, 4))

    def test_position_with_f(self):
        screen = replay(b"\x1b[2;2fy")
        self.assertEqual(screen.find("y"), (1, 1))

    def test_home_with_no_parameters(self):
        screen = replay(b"\x1b[4;4Hz\x1b[HA")
        self.assertEqual(screen.find("A"), (0, 0))
        self.assertEqual(screen.find("z"), (3, 3))

    def test_relative_moves(self):
        # Down 2, right 3, up 1, left 1, then draw.
        screen = replay(b"\x1b[2B\x1b[3C\x1b[1A\x1b[1Dq")
        self.assertEqual(screen.find("q"), (1, 2))

    def test_moves_clamp_at_the_edges(self):
        screen = replay(b"\x1b[99A\x1b[99Dtop")
        self.assertEqual(screen.find("top"), (0, 0))


class TestErase(unittest.TestCase):
    def test_erase_to_end_of_line(self):
        screen = replay(b"abcdef\x1b[1;4H\x1b[K")
        self.assertEqual(screen.line(0), "abc")

    def test_erase_to_start_of_line(self):
        screen = replay(b"abcdef\x1b[1;3H\x1b[1K")
        self.assertEqual(screen.line(0), "   def")

    def test_erase_whole_line(self):
        screen = replay(b"abcdef\x1b[2K")
        self.assertEqual(screen.line(0), "")

    def test_erase_to_end_of_display(self):
        screen = replay(b"aa\r\nbb\r\ncc\x1b[2;2H\x1b[J")
        self.assertEqual(screen.text()[:3], ["aa", "b", ""])

    def test_erase_to_start_of_display(self):
        screen = replay(b"aa\r\nbb\r\ncc\x1b[2;1H\x1b[1J")
        self.assertEqual(screen.text()[:3], ["", " b", "cc"])

    def test_erase_whole_display(self):
        screen = replay(b"aa\r\nbb\x1b[2J")
        self.assertEqual(screen.text(), [""] * 5)

    def test_erase_scrollback_leaves_the_grid_alone(self):
        screen = replay(b"aa\x1b[3J")
        self.assertEqual(screen.line(0), "aa")


class TestSgr(unittest.TestCase):
    def test_set_and_reset(self):
        screen = replay(b"\x1b[31mred\x1b[0mplain")
        self.assertEqual(screen.sgr_of("red"), Sgr(fg=(31,)))
        self.assertEqual(screen.sgr_of("plain"), Sgr())

    def test_multi_parameter(self):
        screen = replay(b"\x1b[1;2;7;33mmix")
        self.assertEqual(
            screen.sgr_of("mix"),
            Sgr(fg=(33,), bold=True, dim=True, reverse=True),
        )

    def test_empty_parameter_is_a_reset(self):
        screen = replay(b"\x1b[1mbold\x1b[mafter")
        self.assertTrue(screen.sgr_of("bold").bold)
        self.assertFalse(screen.sgr_of("after").bold)

    def test_reverse_on_and_off(self):
        screen = replay(b"\x1b[7mbar\x1b[27mrest")
        self.assertTrue(screen.sgr_of("bar").reverse)
        self.assertFalse(screen.sgr_of("rest").reverse)

    def test_bold_off_also_clears_dim(self):
        screen = replay(b"\x1b[1;2mx\x1b[22my")
        self.assertEqual(screen.sgr_of("x"), Sgr(bold=True, dim=True))
        self.assertEqual(screen.sgr_of("y"), Sgr())

    def test_default_foreground(self):
        screen = replay(b"\x1b[31ma\x1b[39mb")
        self.assertEqual(screen.sgr_of("a").fg, (31,))
        self.assertIsNone(screen.sgr_of("b").fg)

    def test_indexed_and_rgb_colours_are_kept_whole(self):
        screen = replay(b"\x1b[38;5;9mi\x1b[0m\x1b[38;2;10;20;30mr\x1b[0m\x1b[42mg")
        self.assertEqual(screen.sgr_of("i").fg, (38, 5, 9))
        self.assertEqual(screen.sgr_of("r").fg, (38, 2, 10, 20, 30))
        self.assertEqual(screen.sgr_of("g").bg, (42,))

    def test_attributes_travel_with_the_cell_not_the_cursor(self):
        screen = replay(b"\x1b[7mA\x1b[0mB")
        self.assertTrue(screen.cell(0, 0).sgr.reverse)
        self.assertFalse(screen.cell(0, 1).sgr.reverse)

    def test_params_round_trip(self):
        screen = replay(b"\x1b[1;7;31mp")
        self.assertEqual(screen.sgr_of("p").params(), [1, 7, 31])

    def test_sgr_of_missing_substring_is_none(self):
        self.assertIsNone(replay(b"abc").sgr_of("zzz"))


class TestRedraw(unittest.TestCase):
    def test_a_later_frame_replaces_an_earlier_one(self):
        first = b"\x1b[H\x1b[2J\x1b[7m one \x1b[0m\r\n two "
        second = b"\x1b[H\x1b[2J two \r\n\x1b[7m three \x1b[0m"
        screen = replay(first + second, rows=3, cols=20)
        self.assertEqual(screen.text(), [" two", " three", ""])
        # The reverse video bar moved with the frame rather than accumulating:
        # exactly one styled run survives, and it is on the second row.
        runs = screen.runs()
        self.assertEqual(len(runs), 1)
        self.assertEqual(runs[0].row, 1)
        self.assertTrue(runs[0].sgr.reverse)

    def test_overwriting_a_line_clears_stale_attributes(self):
        screen = replay(b"\x1b[7mSELECTED\x1b[0m\x1b[H\x1b[Kplain", rows=2, cols=20)
        self.assertEqual(screen.line(0), "plain")
        self.assertFalse(screen.sgr_of("plain").reverse)

    def test_two_reverse_bars_are_visible_as_two_runs(self):
        # The failure this emulator exists to catch: more than one row drawn
        # with the selection bar's reverse video at the same time.
        data = b"\x1b[1;1H\x1b[7mrow one\x1b[0m\x1b[3;1H\x1b[7mrow three\x1b[0m"
        reverse = [run for run in replay(data).runs() if run.sgr.reverse]
        self.assertEqual([(run.row, run.text) for run in reverse],
                         [(0, "row one"), (2, "row three")])


class TestUnhandled(unittest.TestCase):
    def test_known_noise_is_not_counted(self):
        screen = replay(b"\x1b[?25l\x1b[?1049h\x1b[?2004hok\x1b[?25h")
        self.assertEqual(screen.line(0), "ok")
        self.assertEqual(screen.unhandled, 0)

    def test_unknown_sequences_are_counted_and_named(self):
        screen = replay(b"\x1b[42Zvisible")
        self.assertEqual(screen.unhandled, 1)
        self.assertIn("CSI 42 Z", screen.unhandled_summary())
        # Recovery matters as much as the count: the text after the unknown
        # sequence still lands on the grid.
        self.assertEqual(screen.line(0), "visible")

    def test_unknown_sgr_parameter_is_counted(self):
        screen = replay(b"\x1b[1234mx")
        self.assertEqual(screen.unhandled, 1)
        self.assertIn("SGR 1234", screen.unhandled_summary())

    def test_osc_title_is_swallowed_whole(self):
        screen = replay(b"\x1b]0;a window title\x07done")
        self.assertEqual(screen.line(0), "done")
        self.assertEqual(screen.unhandled, 0)


class TestReplies(unittest.TestCase):
    def test_cursor_position_report(self):
        screen = replay(b"\x1b[5;9H\x1b[6n")
        self.assertEqual(screen.take_replies(), b"\x1b[5;9R")
        self.assertEqual(screen.take_replies(), b"")

    def test_device_attributes(self):
        screen = replay(b"\x1b[c")
        self.assertEqual(screen.take_replies(), b"\x1b[?1;2c")


class TestFindAndDump(unittest.TestCase):
    def test_find_returns_the_first_match(self):
        screen = replay(b"xx\r\n  needle")
        self.assertEqual(screen.find("needle"), (1, 2))
        self.assertIsNone(screen.find("haystack"))

    def test_dump_json_is_valid_and_lists_runs(self):
        import json

        screen = replay(b"\x1b[7mbar\x1b[0m tail")
        payload = json.loads(screen.dump_sgr_json())
        self.assertEqual(payload["rows"], 5)
        self.assertEqual(payload["runs"][0]["text"], "bar")
        self.assertEqual(payload["runs"][0]["sgr"], {"reverse": True})

    def test_utf8_split_across_feeds(self):
        screen = Screen(2, 10)
        icon = "●".encode("utf-8")
        screen.feed(icon[:1])
        screen.feed(icon[1:])
        self.assertEqual(screen.line(0), "●")


if __name__ == "__main__":
    unittest.main()
