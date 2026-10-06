#!/usr/bin/env python3
"""Build catalog/gazetteer/us-places.csv, the US gazetteer that `geocode`
compiles in (04 §4.6).

    scripts/build-gazetteer.py [--year 2025] [--zip path/to/YYYY_Gaz_place_national.zip]

Source: the US Census Bureau's Gazetteer Files, "Places" national file
(https://www.census.gov/geographies/reference-files/time-series/geo/gazetteer-files.html),
downloaded from
https://www2.census.gov/geo/docs/maps-data/data/gazetteer/{year}_Gazetteer/{year}_Gaz_place_national.zip
unless --zip names a local copy. It lists every incorporated place and
census designated place (CDP) with an internal point (INTPTLAT, INTPTLONG).
The file is a work of the US government and in the public domain (17 U.S.C.
105).

The output keeps four columns, `state,name,lat,lon`, sorted by state then
name, with a header row:

- `name` drops the legal/statistical area description the Census appends
  ("Abbeville city" → "Abbeville", "Abanda CDP" → "Abanda"; only the one
  suffix its LSAD code names, so "Boise City city" → "Boise City").
  Consolidated governments get the city's own name (see SPECIAL), and
  "Urban Honolulu" is "Honolulu".
- A name with another in parentheses ("San Buenaventura (Ventura)") is
  listed under both, the second only where the state has no place of that
  name already.
- When a state has two places with the same name (a city and a CDP, say),
  an incorporated place wins over a CDP, then the larger land area.
- Coordinates are rounded to 5 decimals (about a metre).

Run it again for a newer year and commit the CSV; nothing else changes.
"""

import argparse
import csv
import io
import sys
import urllib.request
import zipfile
from pathlib import Path

URL = "https://www2.census.gov/geo/docs/maps-data/data/gazetteer/{y}_Gazetteer/{y}_Gaz_place_national.zip"
OUT = Path(__file__).resolve().parent.parent / "catalog" / "gazetteer" / "us-places.csv"

# The suffix each LSAD code adds to NAME.
LSAD_SUFFIX = {
    "21": " borough",
    "25": " city",
    "37": " municipality",
    "43": " town",
    "47": " village",
    "53": " city and borough",
    "55": " comunidad",
    "57": " CDP",
    "62": " zona urbana",
    "CN": " corporation",
}

# Consolidated governments and other names without a plain LSAD suffix:
# the city's name, or None to leave the entry out (a county, not a town).
SPECIAL = {
    ("CT", "Milford city (balance)"): "Milford",
    ("GA", "Athens-Clarke County unified government (balance)"): "Athens",
    ("GA", "Augusta-Richmond County consolidated government (balance)"): "Augusta",
    ("GA", "Cusseta-Chattahoochee County unified government"): "Cusseta",
    ("GA", "Echols County consolidated government"): None,
    ("GA", "Georgetown-Quitman County unified government"): "Georgetown",
    ("GA", "Macon-Bibb County"): "Macon",
    ("FL", "Islamorada, Village of Islands village"): "Islamorada",
    ("GA", "Webster County unified government"): None,
    ("HI", "Urban Honolulu CDP"): "Honolulu",
    ("IN", "Indianapolis city (balance)"): "Indianapolis",
    ("KS", "Greeley County unified government (balance)"): None,
    ("KY", "Lexington-Fayette urban county"): "Lexington",
    ("KY", "Louisville/Jefferson County metro government (balance)"): "Louisville",
    ("MT", "Anaconda-Deer Lodge County"): "Anaconda",
    ("MT", "Butte-Silver Bow (balance)"): "Butte",
    ("NJ", "Princeton"): "Princeton",
    ("NV", "Carson City"): "Carson City",
    ("TN", "Hartsville/Trousdale County"): "Hartsville",
    ("TN", "Lynchburg, Moore County metropolitan government"): "Lynchburg",
    ("TN", "Nashville-Davidson metropolitan government (balance)"): "Nashville",
}


def place_name(state, name, lsad):
    if (state, name) in SPECIAL:
        return SPECIAL[(state, name)]
    suffix = LSAD_SUFFIX.get(lsad)
    if suffix is None or not name.endswith(suffix):
        sys.exit(f"unexpected LSAD {lsad!r} for {state} {name!r}: add it to SPECIAL")
    return name[: -len(suffix)]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--year", default="2025")
    ap.add_argument("--zip", help="a local copy of the zip instead of downloading it")
    ap.add_argument("--out", default=str(OUT))
    args = ap.parse_args()

    if args.zip:
        data = Path(args.zip).read_bytes()
    else:
        with urllib.request.urlopen(URL.format(y=args.year), timeout=120) as r:
            data = r.read()
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        (member,) = [n for n in z.namelist() if n.endswith(".txt")]
        text = z.read(member).decode("utf-8")
    first = text.splitlines()[0]
    delimiter = "|" if "|" in first else "\t"
    best = {}
    for row in csv.DictReader(io.StringIO(text), delimiter=delimiter):
        row = {k.strip(): v.strip() for k, v in row.items()}
        state = row["USPS"]
        name = place_name(state, row["NAME"], row["LSAD"])
        if name is None:
            continue
        rank = (row["LSAD"] != "57", int(row["ALAND"] or 0))
        lat = round(float(row["INTPTLAT"]), 5)
        lon = round(float(row["INTPTLONG"]), 5)
        names = [name]
        if name.endswith(")") and " (" in name:
            main, alt = name[:-1].split(" (", 1)
            names = [main, alt]
            # An alternative name ranks below any place called that.
            alt_rank = (False, -1)
        for i, n in enumerate(names):
            if "," in n or '"' in n:
                sys.exit(f"{state} {n!r}: a comma or quote in a name; add it to SPECIAL")
            r = rank if i == 0 else alt_rank
            key = (state, n)
            if key not in best or r > best[key][0]:
                best[key] = (r, lat, lon)

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w", encoding="utf-8", newline="") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(["state", "name", "lat", "lon"])
        for (state, name), (_, lat, lon) in sorted(best.items()):
            w.writerow([state, name, f"{lat:.5f}", f"{lon:.5f}"])
    print(f"{out}: {len(best)} places", file=sys.stderr)


if __name__ == "__main__":
    main()
