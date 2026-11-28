# Data loss codes

RustGIS never drops or changes data silently. A notice says what happened, how much and where.
A notice that reports a loss also carries one of the codes on this page. Each notice links to its
entry here, which says what the code means and what to do about it.

A code is never translated, renamed or reused, so a script can match on it.
[`rustgis-cli run --strict`](../cli.md#strict-runs) stops at a code that no `--allow` accepts,
Python raises it as [`rustgis.DataLossWarning`](../python.md), and the [MCP server](../mcp.md)
returns it under `losses`.

Not every loss has a code yet. Losses said without one are only notices, and don't stop a strict
run.

## Dropped

Dropped means data was left out and is not in the result.

### `dropped.legend` { #dropped.legend }

A map service offered legend pictures that RustGIS does not show, so they were left out. These are pictures with addresses that fail RustGIS's checks, are in another format, or past the limit on legends per layer.

**Where you see it:** adding a map service layer (WMS or similar) that offers legends.

**What to do:** Use a service with short HTTP or HTTPS addresses for its legend pictures.

### `dropped.ring` { #dropped.ring }

A polygon ring was left out. Rings that could not be read, or that have no area, are skipped, and a format with no holes keeps only the outer edge of each polygon.

**Where you see it:** reading GeoRSS, GML or MapInfo data, or feature service answers in PBF; exporting polygons to GPX (their holes are left out).

**What to do:** Correct the unreadable polygon ring in the source, or write to a format that holds holes, such as GeoPackage.

### `dropped.extra-attributes` { #dropped.extra-attributes }

A row had more attribute values than the layer has fields, and the extra values were not kept.

**Where you see it:** reading a CSV file with rows longer than its header; the Repair Geometry and Check Geometry tools.

**What to do:** Add fields for the extra attribute values or remove those values from the input.

### `dropped.time` { #dropped.time }

A date-time was written to a date field, which keeps only the day, so the time of day was lost.

**Where you see it:** Calculate Field; editing or pasting table values into a date field; the Update tool.

**What to do:** Write to a date-time field to keep the time of day.

### `dropped.value` { #dropped.value }

A value could not be kept and was read or written as empty (null). Examples are spreadsheet error cells, text in a date or yes/no column, and a result that is not a number.

**Where you see it:** reading a spreadsheet, Parquet, PostGIS or table-based source; reading DTED or GRIB rasters; Calculate Field.

**What to do:** Correct the value or use a destination field that can hold it.

### `dropped.calculation-result` { #dropped.calculation-result }

A calculation gave no result for some rows, so the field was left empty there. The input may have no place that can be given in longitude and latitude, or the result may be too large to hold.

**Where you see it:** Calculate Field; Calculate Geometry Attributes; table statistics; Profile Along Selected Lines.

**What to do:** Correct the expression or check the input rows named in the notice.

### `dropped.zm` { #dropped.zm }

Z (height) or M (measure) values were not kept. RustGIS keeps x and y only.

**Where you see it:** reading KML, CSV or WKT, or a table read through DuckDB or GDAL; exporting a layer whose Z or M values were dropped when it was read.

**What to do:** Keep the source file where heights or measures matter: RustGIS keeps x and y only.

### `dropped.crs-name` { #dropped.crs-name }

A coordinate system's name or definition could not be written with the data, so the file holds the coordinates without it.

**Where you see it:** exporting to a Shapefile (no .prj file) or to CSV; exporting layers on another body than the Earth; reading Zarr stores.

**What to do:** Keep the source file's coordinate system definition with the export, or export in WGS 84, which every format can name.

### `dropped.feature` { #dropped.feature }

Whole features, rows or items were left out because they could not be read or are not supported.

**Where you see it:** reading GeoRSS, OpenStreetMap, spreadsheet, GRIB or point-cloud (LAS and LAZ) files.

**What to do:** Correct those features in the source, or read the source in the way the notice suggests.

### `dropped.geometry` { #dropped.geometry }

A feature's geometry could not be read or written. The feature was kept without its place, or the place was left out.

**Where you see it:** reading CSV with WKT, GPX, TopoJSON or MapInfo data; exporting layers with a place the target coordinate system cannot hold; Repair Geometry.

**What to do:** Correct the geometry in the source, or give the coordinate system its coordinates are in.

### `dropped.over-limit` { #dropped.over-limit }

RustGIS read only part of the data because it is past a size limit.

**Where you see it:** reading point-cloud (LAS and LAZ), DXF, KML or photo data, or a raster with a very long list of class names.

**What to do:** Read less at once, such as by filtering or clipping the source first.

## Changed

Changed means data is still there but was altered, such as cut short or rounded.

### `changed.truncated` { #changed.truncated }

A number lost its fractional part because the field holds whole numbers.

**Where you see it:** Calculate Field; editing table values; the Update tool.

**What to do:** Use a decimal field to keep the fractional part.

### `changed.field-name` { #changed.field-name }

A field name was changed to fit the format it was written to.

**Where you see it:** exporting to Shapefile or GeoPackage.

**What to do:** Use field names the format holds: at most 10 bytes and unique ignoring case for a Shapefile, unique ignoring case for a GeoPackage.

### `changed.cut` { #changed.cut }

Text was cut short to fit a format or a limit.

**Where you see it:** exporting to Shapefile (254 bytes per value); labels and annotations; reading raster band or class names.

**What to do:** Shorten the text to what the format holds (254 bytes for a Shapefile; nothing past a NUL character), or write to a format without the limit, such as GeoPackage.

### `changed.body` { #changed.body }

Longitude and latitude on a body other than the Earth were written as Earth coordinates, because the format cannot say which body they are on.

**Where you see it:** exporting layers in a planetary coordinate system.

**What to do:** Export in the layer's own coordinate system, which keeps its body.

### `changed.encoding` { #changed.encoding }

Text that is not valid in the encoding it was read with was read with a replacement character in its place.

**Where you see it:** reading a Shapefile or dBASE file, a DXF drawing, or feature service answers in PBF.

**What to do:** Put a .cpg file naming the file's own encoding beside it.

### `changed.rounded` { #changed.rounded }

A number or time was rounded to what the format or the field holds.

**Where you see it:** exporting to Shapefile or GeoPackage; reading spreadsheets or File Geodatabases; Calculate Field and table statistics.

**What to do:** Keep the source file where finer values matter, or write to a format that holds them (times finer than the format keeps, numbers past a dBase field's precision, or whole numbers past 2^53 in a decimal field).

### `changed.position` { #changed.position }

Positions were moved to fit the coordinate system they were written in, for example vertices beyond 85.05 degrees north or south in Web Mercator.

**Where you see it:** exporting in Web Mercator.

**What to do:** Export in a coordinate system that holds the positions, such as WGS 84.

## Approximated

Approximated means data was kept as a close stand-in for the original.

### `approximated.datum` { #approximated.datum }

A datum RustGIS does not shift was treated as WGS 84, so places may be a little off.

**Where you see it:** reading DTED rasters; exporting a layer that is not in WGS 84 to a format that holds only longitude and latitude.

**What to do:** Export in the layer's own coordinate system, or give that system with a known transformation to WGS 84.

### `approximated.curve` { #approximated.curve }

Curves, circles or rotated boxes were drawn or written as straight segments.

**Where you see it:** reading GML or File Geodatabase data; the Buffer tool; exporting circles and rotated boxes drawn in the session.

**What to do:** Keep the source file where exact curves matter: RustGIS draws curves as straight segments.

## Assumed

Assumed means RustGIS had to guess something the data did not say, so the result may be wrong if the guess is.

### `assumed.null-marker` { #assumed.null-marker }

A value written as the text `<Null>`, which some programs write for a missing value, was read as
empty (null).

**Where you see it:** reading a Shapefile, GeoPackage, GeoJSON, CSV or Parquet file, or point-cloud
data.

**What to do:** Write the value another way if the text Null is meant as text, not as an empty value.

### `assumed.crs` { #assumed.crs }

The file does not say which coordinate system its coordinates are in, or says one RustGIS cannot use, so RustGIS read them as longitude and latitude (WGS 84).

**Where you see it:** reading a Shapefile with an unusable .prj file, GeoPackage, PostGIS or SQL Server data, Zarr stores and point-cloud files.

**What to do:** Give the coordinate system the coordinates are in ("crs" when adding the data, or Define Projection).

### `assumed.encoding` { #assumed.encoding }

The file does not name its text encoding, or names one RustGIS does not read, so RustGIS assumed one (UTF-8, or Latin-1 for a DXF).

**Where you see it:** reading a Shapefile or dBASE file without a usable .cpg file, or a DXF drawing without a known code page.

**What to do:** Give the file's encoding in a form RustGIS reads: a .cpg beside a Shapefile or dBASE file naming UTF-8 or its Windows code page, or a DXF saved as UTF-8.
