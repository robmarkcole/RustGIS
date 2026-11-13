# Test data for rustgis-render

- `test-cff.otf`: a CFF (OpenType .otf) face made for the PDF font tests (#1815), so that they need
  no third-party font. It holds `.notdef` and three glyphs of distinct shapes and widths: `A` a
  square (600 units wide), `B` a triangle (700) and `C` two bars (800), 1000 units per em,
  `fsType` 0. It was made with fontTools 4 by the script below, and is part of RustGIS under its
  licence.

```python
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.t2CharStringPen import T2CharStringPen

fb = FontBuilder(1000, isTTF=False)
order = [".notdef", "A", "B", "C"]
fb.setupGlyphOrder(order)
fb.setupCharacterMap({ord("A"): "A", ord("B"): "B", ord("C"): "C"})
adv = {".notdef": 500, "A": 600, "B": 700, "C": 800}

def draw(name):
    p = T2CharStringPen(adv[name], None)
    if name == "A":
        p.moveTo((50, 0)); p.lineTo((550, 0)); p.lineTo((550, 500)); p.lineTo((50, 500)); p.closePath()
    elif name == "B":
        p.moveTo((50, 0)); p.lineTo((650, 0)); p.lineTo((350, 700)); p.closePath()
    elif name == "C":
        p.moveTo((50, 0)); p.lineTo((250, 0)); p.lineTo((250, 600)); p.lineTo((50, 600)); p.closePath()
        p.moveTo((500, 0)); p.lineTo((750, 0)); p.lineTo((750, 300)); p.lineTo((500, 300)); p.closePath()
    return p.getCharString()

fb.setupCFF("RustGISTestCFF", {"FullName": "RustGIS Test CFF"}, {g: draw(g) for g in order}, {})
fb.setupHorizontalMetrics({g: (adv[g], 50) for g in order})
fb.setupHorizontalHeader(ascent=800, descent=-200)
fb.setupNameTable({"familyName": "RustGIS Test CFF", "styleName": "Regular"})
fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200, fsType=0)
fb.setupPost()
fb.save("test-cff.otf")
```
