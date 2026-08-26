#!/usr/bin/env python3
"""Render one ANSI-coloured line to a standalone SVG.

Reads the statusline on stdin and emits an SVG whose text is real text: it stays
selectable, greppable and legible in a diff, unlike a screenshot.
"""
import re
import sys
from xml.sax.saxutils import escape

# One palette entry per SGR colour the statusline actually emits.
PALETTE = {
    "30": "#1f2430", "31": "#f7768e", "32": "#9ece6a", "33": "#e0af68",
    "34": "#7aa2f7", "35": "#bb9af7", "36": "#7dcfff", "37": "#c0caf5",
    "90": "#565f89", "91": "#ff7a93", "92": "#b9f27c", "93": "#ff9e64",
    "94": "#7da6ff", "95": "#bb9af7", "96": "#0db9d7", "97": "#ffffff",
}
DEFAULT_FG = "#c0caf5"
BACKGROUND = "#1a1b26"

FONT = "ui-monospace, 'SF Mono', 'JetBrains Mono', 'Fira Code', Menlo, Consolas, monospace"
FONT_SIZE = 15
# Advance width of the monospace cell at FONT_SIZE, and the emoji's double width.
CELL_W = FONT_SIZE * 0.60
LINE_H = FONT_SIZE * 1.6
PAD_X, PAD_Y = 16, 12

SGR = re.compile(r"\x1b\[([0-9;]*)m")


def spans(line):
    """Split an ANSI line into (text, colour) runs."""
    out, pos, colour = [], 0, None
    for m in SGR.finditer(line):
        if m.start() > pos:
            out.append((line[pos:m.start()], colour))
        for code in (m.group(1) or "0").split(";"):
            if code in ("0", "39", ""):
                colour = None
            elif code in PALETTE:
                colour = PALETTE[code]
        pos = m.end()
    if pos < len(line):
        out.append((line[pos:], colour))
    return [(t, c) for t, c in out if t]


def width(text):
    """Column estimate. Colour-emoji fonts advance wider than two cells, and
    under-counting clips the line, so emoji are charged generously."""
    n = 0.0
    for ch in text:
        o = ord(ch)
        if ch == "​":  # zero-width joiner the burn-rate emoji carries
            continue
        if o >= 0x1F300 or 0x2600 <= o <= 0x27BF:
            n += 2.6
        elif o > 0x2500 and not (0x2000 <= o <= 0x206F):
            n += 2
        else:
            n += 1
    return n


def main():
    line = sys.stdin.read().rstrip("\n")
    runs = spans(line)
    cols = sum(width(t) for t, _ in runs)
    # Deliberately generous: trailing space costs nothing, while an under-estimate
    # runs the text off the right edge in whatever font the viewer happens to use.
    w = int(cols * CELL_W * 1.08 + PAD_X * 2)
    h = int(LINE_H + PAD_Y * 2)

    # No width/height attributes: the viewBox alone lets the image scale to its
    # container, so a font whose advances exceed the estimate shrinks the line to
    # fit rather than clipping it off the right edge.
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" '
        f'preserveAspectRatio="xMinYMid meet" '
        f'role="img" aria-label="ccusage-statusline-rs example output">',
        f'<rect width="{w}" height="{h}" rx="6" fill="{BACKGROUND}"/>',
        f'<text x="{PAD_X}" y="{PAD_Y + FONT_SIZE}" font-family="{FONT}" '
        f'font-size="{FONT_SIZE}" fill="{DEFAULT_FG}" xml:space="preserve">',
    ]
    # Spans flow: no per-span x, so each renderer applies its own font metrics and
    # the colours cannot drift off their tokens when emoji advance differently.
    #
    # Emitted as one unbroken run. Under xml:space="preserve" a newline between
    # two tspans is itself content, and renders as a space inside the line.
    spans_markup = "".join(
        f'<tspan{f" fill=\"{colour}\"" if colour else ""}>{escape(text)}</tspan>'
        for text, colour in runs
    )
    # The opening <text> tag, the spans and </text> stay on one line for the same
    # reason: any newline between them is preserved content.
    parts[-1] += spans_markup + "</text>"
    parts.append("</svg>")
    print("\n".join(parts))


if __name__ == "__main__":
    main()
