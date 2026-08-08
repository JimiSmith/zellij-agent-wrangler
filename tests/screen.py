"""An ANSI terminal emulator that keeps enough state to assert on.

The sidebar plugin's output only exists as escape sequences on a terminal, so a
test cannot read it back without replaying those sequences into a grid. This
module is that grid: every cell remembers both its character and the SGR
attributes that were active when it was drawn, because several of the plugin's
behaviours (the selection bar, an agent's status colour) are carried entirely in
attributes and are invisible in the text alone.

Fidelity is deliberately partial. Sequences that do not move the cursor or
change a cell are recorded and skipped rather than emulated, and anything the
parser does not recognise is counted in `unhandled` so a test can notice that
the emulator has fallen behind what the program under test actually emits.
"""

import codecs
import json
from dataclasses import dataclass, replace

# Sequences that reach a real terminal but leave the visible grid alone. They
# are skipped without being counted as unhandled, so `unhandled` stays a useful
# signal instead of being drowned by zellij's start-up mode setting.
_IGNORED_CSI_FINALS = frozenset("hlnctqrpx")

_DEFAULT_ROWS = 24
_DEFAULT_COLS = 80
_TAB_WIDTH = 8


@dataclass(frozen=True)
class Sgr:
    """The character attributes active on a cell.

    `fg` and `bg` hold the whole colour introducer as written, so `(31,)` is
    plain red and `(38, 5, 9)` is palette index 9. Keeping the parameters rather
    than a resolved colour means a test asserts on what the program emitted,
    which is what a rendering bug actually changes.
    """

    fg: tuple = None
    bg: tuple = None
    bold: bool = False
    dim: bool = False
    reverse: bool = False
    underline: bool = False
    italic: bool = False

    def is_default(self):
        return self == DEFAULT_SGR

    def params(self):
        """The SGR parameters that would reproduce this state from a reset."""
        out = []
        if self.bold:
            out.append(1)
        if self.dim:
            out.append(2)
        if self.italic:
            out.append(3)
        if self.underline:
            out.append(4)
        if self.reverse:
            out.append(7)
        if self.fg is not None:
            out.extend(self.fg)
        if self.bg is not None:
            out.extend(self.bg)
        return out

    def as_dict(self):
        """A JSON-friendly view, carrying only the attributes that are set."""
        out = {}
        if self.fg is not None:
            out["fg"] = list(self.fg)
        if self.bg is not None:
            out["bg"] = list(self.bg)
        for name in ("bold", "dim", "reverse", "underline", "italic"):
            if getattr(self, name):
                out[name] = True
        return out


DEFAULT_SGR = Sgr()


@dataclass(frozen=True)
class Cell:
    char: str = " "
    sgr: Sgr = DEFAULT_SGR


BLANK = Cell()


@dataclass(frozen=True)
class Run:
    """A stretch of cells on one row sharing a single set of attributes."""

    row: int
    col: int
    text: str
    sgr: Sgr

    def as_dict(self):
        return {
            "row": self.row,
            "col": self.col,
            "text": self.text,
            "sgr": self.sgr.as_dict(),
        }


class Screen:
    def __init__(self, rows=_DEFAULT_ROWS, cols=_DEFAULT_COLS):
        self.rows = rows
        self.cols = cols
        self.unhandled = 0
        # Keyed by a short description of the sequence, so a test that trips the
        # counter can report which sequence it was rather than only how many.
        self.unhandled_seen = {}
        # Answers owed to the program under test for queries such as "where is
        # the cursor". The screen cannot write to the pty itself, so the driver
        # drains this after every feed; leaving it undrained can wedge a program
        # that blocks on the reply.
        self.replies = bytearray()
        self._decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        self.reset()

    # -- state ------------------------------------------------------------

    def reset(self):
        self.grid = [[BLANK] * self.cols for _ in range(self.rows)]
        self.row = 0
        self.col = 0
        self.sgr = DEFAULT_SGR
        self._saved = (0, 0, DEFAULT_SGR)
        self._scroll_top = 0
        self._scroll_bottom = self.rows - 1
        self._state = "ground"
        self._csi = ""
        self._pending = ""

    # -- reading ----------------------------------------------------------

    def line(self, n):
        """Row `n` as text, with trailing blanks removed."""
        return "".join(cell.char for cell in self.grid[n]).rstrip()

    def text(self):
        return [self.line(n) for n in range(self.rows)]

    def cell(self, row, col):
        return self.grid[row][col]

    def find(self, substring):
        """The (row, col) of the first occurrence of `substring`, or None."""
        for n in range(self.rows):
            col = self.line(n).find(substring)
            if col >= 0:
                return (n, col)
        return None

    def contains(self, substring):
        return self.find(substring) is not None

    def sgr_of(self, substring):
        """The attributes on the first cell of `substring`, or None if absent.

        This is how a test says "that row's icon is red": find the glyph, read
        the attributes that were active when it was drawn.
        """
        found = self.find(substring)
        if found is None:
            return None
        row, col = found
        return self.grid[row][col].sgr

    def runs(self, include_default=False):
        """Contiguous same-attribute stretches of text, row by row.

        Trailing blank cells at default attributes are dropped so a dump stays
        readable. With `include_default` false this is a list of everything on
        screen that is styled at all, which is what makes "exactly one reverse
        video bar is drawn" a one-line assertion.
        """
        out = []
        for row in range(self.rows):
            cells = self.grid[row]
            last = len(cells)
            while last > 0 and cells[last - 1] == BLANK:
                last -= 1
            col = 0
            while col < last:
                sgr = cells[col].sgr
                end = col
                while end < last and cells[end].sgr == sgr:
                    end += 1
                if include_default or not sgr.is_default():
                    text = "".join(c.char for c in cells[col:end])
                    out.append(Run(row, col, text, sgr))
                col = end
        return out

    def unhandled_summary(self):
        """Unrecognised sequences and how often each was seen."""
        return dict(self.unhandled_seen)

    def dump_text(self):
        return "\n".join(self.text())

    def dump_sgr_json(self):
        payload = {
            "rows": self.rows,
            "cols": self.cols,
            "cursor": {"row": self.row, "col": self.col},
            "unhandled": self.unhandled,
            "unhandled_seen": self.unhandled_summary(),
            "runs": [run.as_dict() for run in self.runs()],
        }
        return json.dumps(payload, indent=2, ensure_ascii=False)

    def take_replies(self):
        """Hand over (and clear) the bytes owed to the program under test."""
        out = bytes(self.replies)
        self.replies.clear()
        return out

    # -- writing ----------------------------------------------------------

    def feed(self, data):
        if isinstance(data, (bytes, bytearray)):
            data = self._decoder.decode(bytes(data))
        for ch in data:
            self._feed_char(ch)

    def _feed_char(self, ch):
        if self._state == "ground":
            self._ground(ch)
        elif self._state == "escape":
            self._escape(ch)
        elif self._state == "csi":
            self._csi_char(ch)
        elif self._state == "string":
            self._string_char(ch)
        elif self._state == "charset":
            # The single character naming the character set, which this
            # emulator has no use for.
            self._state = "ground"

    def _ground(self, ch):
        if ch == "\x1b":
            self._state = "escape"
        elif ch == "\r":
            self.col = 0
        elif ch == "\n" or ch == "\x0b" or ch == "\x0c":
            self._index()
        elif ch == "\b":
            self.col = max(0, self.col - 1)
        elif ch == "\t":
            self.col = min(self.cols - 1, (self.col // _TAB_WIDTH + 1) * _TAB_WIDTH)
        elif ch == "\x07":
            pass
        elif ch < " ":
            self._note_unhandled("C0 %02x" % ord(ch))
        else:
            self._put(ch)

    def _escape(self, ch):
        if ch == "[":
            self._csi = ""
            self._state = "csi"
        elif ch in "]P^_X":
            # OSC and the other string-terminated sequences: the payload is
            # inspected only far enough to answer colour queries.
            self._pending = ""
            self._state = "string"
        elif ch == "(" or ch == ")" or ch == "*" or ch == "+":
            self._state = "charset"
        elif ch == "7":
            self._saved = (self.row, self.col, self.sgr)
            self._state = "ground"
        elif ch == "8":
            self.row, self.col, self.sgr = self._saved
            self._state = "ground"
        elif ch == "D":
            self._index()
            self._state = "ground"
        elif ch == "E":
            self._index()
            self.col = 0
            self._state = "ground"
        elif ch == "M":
            self._reverse_index()
            self._state = "ground"
        elif ch == "c":
            self.reset()
        elif ch in "=><\\":
            self._state = "ground"
        else:
            self._note_unhandled("ESC %s" % ch)
            self._state = "ground"

    def _string_char(self, ch):
        # Both terminators are accepted: BEL is what most programs actually
        # send, ST is what the standard asks for.
        if ch == "\x07":
            self._string_done()
        elif ch == "\x1b":
            self._pending += "\x1b"
        elif ch == "\\" and self._pending.endswith("\x1b"):
            self._pending = self._pending[:-1]
            self._string_done()
        else:
            self._pending += ch

    def _string_done(self):
        body = self._pending
        self._pending = ""
        self._state = "ground"
        # A program that asks for the foreground or background colour may wait
        # for the answer before drawing anything at all.
        if body.startswith("10;?"):
            self.replies += b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"
        elif body.startswith("11;?"):
            self.replies += b"\x1b]11;rgb:0000/0000/0000\x1b\\"

    def _csi_char(self, ch):
        if "\x40" <= ch <= "\x7e":
            self._csi_final(ch)
            self._state = "ground"
        else:
            self._csi += ch

    # -- CSI --------------------------------------------------------------

    def _params(self, default=0):
        body = self._csi.lstrip("?<>=!")
        if not body:
            return []
        out = []
        for part in body.split(";"):
            head = part.split(":")[0]
            out.append(int(head) if head.isdigit() else default)
        return out

    def _param(self, index, default):
        params = self._params()
        if index < len(params) and params[index] != 0:
            return params[index]
        return default

    def _csi_final(self, final):
        # A tuple, not a string: `"" in "?<>=!"` is true, which would make every
        # parameterless sequence look private.
        private = self._csi[:1] in ("?", "<", ">", "=", "!")

        if final == "m" and not private:
            self._sgr(self._params())
        elif final in "Hf":
            self.row = self._clamp_row(self._param(0, 1) - 1)
            self.col = self._clamp_col(self._param(1, 1) - 1)
        elif final == "A":
            self.row = self._clamp_row(self.row - self._param(0, 1))
        elif final == "B":
            self.row = self._clamp_row(self.row + self._param(0, 1))
        elif final == "C":
            self.col = self._clamp_col(self.col + self._param(0, 1))
        elif final == "D":
            self.col = self._clamp_col(self.col - self._param(0, 1))
        elif final == "E":
            self.row = self._clamp_row(self.row + self._param(0, 1))
            self.col = 0
        elif final == "F":
            self.row = self._clamp_row(self.row - self._param(0, 1))
            self.col = 0
        elif final in "G`":
            self.col = self._clamp_col(self._param(0, 1) - 1)
        elif final == "d":
            self.row = self._clamp_row(self._param(0, 1) - 1)
        elif final == "J":
            self._erase_display(self._params()[0] if self._params() else 0)
        elif final == "K":
            self._erase_line(self._params()[0] if self._params() else 0)
        elif final == "X":
            self._erase_cells(self.row, self.col, self._param(0, 1))
        elif final == "P":
            self._delete_chars(self._param(0, 1))
        elif final == "@":
            self._insert_chars(self._param(0, 1))
        elif final == "L":
            self._insert_lines(self._param(0, 1))
        elif final == "M":
            self._delete_lines(self._param(0, 1))
        elif final == "S":
            for _ in range(self._param(0, 1)):
                self._scroll_up()
        elif final == "T":
            for _ in range(self._param(0, 1)):
                self._scroll_down()
        elif final == "r" and not private:
            top = self._param(0, 1) - 1
            bottom = self._param(1, self.rows) - 1
            if 0 <= top < bottom < self.rows:
                self._scroll_top = top
                self._scroll_bottom = bottom
                self.row = top
                self.col = 0
        elif final == "s":
            self._saved = (self.row, self.col, self.sgr)
        elif final == "u":
            self.row, self.col, self.sgr = self._saved
        elif final == "n" and not private:
            self._device_status(self._params())
        elif final == "c":
            # Device attributes: claim to be a plain VT100 with colour.
            self.replies += b"\x1b[>0;95;0c" if private else b"\x1b[?1;2c"
        elif final in _IGNORED_CSI_FINALS:
            pass
        else:
            self._note_unhandled("CSI %s %s" % (self._csi, final))

    def _device_status(self, params):
        if params[:1] == [6]:
            self.replies += b"\x1b[%d;%dR" % (self.row + 1, self.col + 1)
        elif params[:1] == [5]:
            self.replies += b"\x1b[0n"

    def _sgr(self, params):
        if not params:
            params = [0]
        index = 0
        while index < len(params):
            code = params[index]
            index += 1
            if code == 0:
                self.sgr = DEFAULT_SGR
            elif code == 1:
                self.sgr = replace(self.sgr, bold=True)
            elif code == 2:
                self.sgr = replace(self.sgr, dim=True)
            elif code == 3:
                self.sgr = replace(self.sgr, italic=True)
            elif code == 4:
                self.sgr = replace(self.sgr, underline=True)
            elif code == 7:
                self.sgr = replace(self.sgr, reverse=True)
            elif code == 21 or code == 22:
                self.sgr = replace(self.sgr, bold=False, dim=False)
            elif code == 23:
                self.sgr = replace(self.sgr, italic=False)
            elif code == 24:
                self.sgr = replace(self.sgr, underline=False)
            elif code == 27:
                self.sgr = replace(self.sgr, reverse=False)
            elif 30 <= code <= 37 or 90 <= code <= 97:
                self.sgr = replace(self.sgr, fg=(code,))
            elif 40 <= code <= 47 or 100 <= code <= 107:
                self.sgr = replace(self.sgr, bg=(code,))
            elif code == 39:
                self.sgr = replace(self.sgr, fg=None)
            elif code == 49:
                self.sgr = replace(self.sgr, bg=None)
            elif code == 38 or code == 48:
                colour, index = self._extended_colour(params, index, code)
                if code == 38:
                    self.sgr = replace(self.sgr, fg=colour)
                else:
                    self.sgr = replace(self.sgr, bg=colour)
            elif code in (5, 6, 8, 9, 25, 28, 29, 53, 55, 59, 73, 74, 75):
                # Blink, conceal, strikethrough and friends: recognised so they
                # do not inflate the unhandled count, but not tracked per cell.
                pass
            else:
                self._note_unhandled("SGR %d" % code)

    def _extended_colour(self, params, index, introducer):
        """Consume a 38/48 colour and return it with the new parameter index."""
        if index < len(params) and params[index] == 5:
            return (introducer, 5) + tuple(params[index + 1 : index + 2]), index + 2
        if index < len(params) and params[index] == 2:
            return (introducer, 2) + tuple(params[index + 1 : index + 4]), index + 4
        return None, index

    # -- grid -------------------------------------------------------------

    def _clamp_row(self, row):
        return max(0, min(self.rows - 1, row))

    def _clamp_col(self, col):
        return max(0, min(self.cols - 1, col))

    def _put(self, ch):
        if self.col >= self.cols:
            self.col = 0
            self._index()
        self.grid[self.row][self.col] = Cell(ch, self.sgr)
        self.col += 1

    def _index(self):
        if self.row == self._scroll_bottom:
            self._scroll_up()
        else:
            self.row = min(self.rows - 1, self.row + 1)

    def _reverse_index(self):
        if self.row == self._scroll_top:
            self._scroll_down()
        else:
            self.row = max(0, self.row - 1)

    def _scroll_up(self):
        del self.grid[self._scroll_top]
        self.grid.insert(self._scroll_bottom, [BLANK] * self.cols)

    def _scroll_down(self):
        del self.grid[self._scroll_bottom]
        self.grid.insert(self._scroll_top, [BLANK] * self.cols)

    def _erase_cells(self, row, col, count):
        for n in range(col, min(self.cols, col + count)):
            self.grid[row][n] = BLANK

    def _erase_line(self, mode):
        if mode == 0:
            self._erase_cells(self.row, self.col, self.cols)
        elif mode == 1:
            self._erase_cells(self.row, 0, self.col + 1)
        elif mode == 2:
            self._erase_cells(self.row, 0, self.cols)
        else:
            self._note_unhandled("CSI %d K" % mode)

    def _erase_display(self, mode):
        if mode == 0:
            self._erase_cells(self.row, self.col, self.cols)
            for row in range(self.row + 1, self.rows):
                self.grid[row] = [BLANK] * self.cols
        elif mode == 1:
            for row in range(0, self.row):
                self.grid[row] = [BLANK] * self.cols
            self._erase_cells(self.row, 0, self.col + 1)
        elif mode == 2:
            self.grid = [[BLANK] * self.cols for _ in range(self.rows)]
        elif mode == 3:
            # Erases the scrollback, which this emulator does not keep. The
            # visible grid must survive it.
            pass
        else:
            self._note_unhandled("CSI %d J" % mode)

    def _delete_chars(self, count):
        row = self.grid[self.row]
        del row[self.col : self.col + count]
        row.extend([BLANK] * (self.cols - len(row)))

    def _insert_chars(self, count):
        row = self.grid[self.row]
        for _ in range(count):
            row.insert(self.col, BLANK)
        del row[self.cols :]

    def _insert_lines(self, count):
        for _ in range(count):
            del self.grid[self._scroll_bottom]
            self.grid.insert(self.row, [BLANK] * self.cols)

    def _delete_lines(self, count):
        for _ in range(count):
            del self.grid[self.row]
            self.grid.insert(self._scroll_bottom, [BLANK] * self.cols)

    def _note_unhandled(self, description):
        self.unhandled += 1
        self.unhandled_seen[description] = self.unhandled_seen.get(description, 0) + 1
