# Term skew: full output for seven searches

**Status:** measured; nothing changed · **Date:** 1 October 2026 · **Measured on:** index `pages-v20260929-4`, with `cargo run -p usnm-core --example term_skew` as given in [design 11 §11.5](../design/11-term-geographic-skew.md#115-evidence) · **Code:** `crates/usnm-core/examples/term_skew.rs` · **Issues:** #62

The complete `term_skew` output behind the corpus figures in design 11. It moved here from that document's §11.13 on 10 November 2026; the summary figures stay in the design document.

<details>
<summary><code>term_skew</code> output for the seven searches</summary>

```
## Corpus (pages-v20260929-4)
places 398  pages 6557925  median pages/place 1764  p90 18728  max 1990131  top 10 places hold 60.7% of pages
pages per title, by place: p10 4  median 805  p90 8728  min 1  max 67594  (p90/p10 = 2182x)
spearman(pages, titles) across places = 0.437  places with one title 195
states and territories with pages 42
years with pages 198  of which with pages from fewer than 5 states 60  first year with 5+ states 1776

## yellow_fever: "yellow fever"  1751-05-09 to 1963-12-31 by month (2552 buckets)
hits 66769  pages 6557925  places with pages 398  with hits 265  coverage cells 61770  scoring 11828 us
raw hits vs pages: spearman 0.825 (all places)  R^2 of log-log 0.561 (places with hits)
raw hits vs titles (whole corpus): spearman 0.381  R^2 of log-log 0.217
share-of-pages colour today: max 1.0000 at Little Rock Ark., AR (1 pages, 1 hits); 2 of 265 places with hits are in the top half of the scale
dispersion phi 3.08 from 144 places
prior: alpha 2.321  mean 0.846  (prior sd of lift 0.56)  scored with shape 2.305
90% interval above 1 / below 1 / includes 1: 23 / 91 / 284  (phi = 1: 42 / 138)
same search by year: phi 3.08 from 144 places, above/below 24 / 87, 5 places change flag
top 10 that are also among the 10 places with most pages: raw hits 7  share of pages 0  estimate 2  lower bound 2
top 10 by share of pages: 9 have fewer pages than the median place (1764)
without time standardization: 162 places change flag; largest gap Miami, FL expected 18.0 with time vs 280.8 without (0.06x)
with the place in its own reference rate: 1 places change flag; Washington, DC raw lift 1.093 vs 1.116 against the others
top 5 by raw hits:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  New-York, NY | 699264 | 15124 | 13236.0 | 1.14 | 1.14 | 1.12 to 1.17 | 1.00
  Washington, DC | 1990131 | 12658 | 11337.8 | 1.12 | 1.12 | 1.09 to 1.15 | 1.00
  Wilmington, DE | 192859 | 3413 | 3373.4 | 1.01 | 1.01 | 0.96 to 1.06 | 0.64
  Birmingham, AL | 164346 | 3316 | 1207.4 | 2.75 | 2.73 | 2.60 to 2.87 | 1.00
  Washington City, DC | 118302 | 3114 | 3751.7 | 0.83 | 0.83 | 0.79 to 0.87 | 0.00
top 5 by raw lift:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Little Rock Ark., AR | 1 | 1 | 0.0 | 21.68 | 0.84 | 0.23 to 2.09 | 0.39
  Langston City, OK | 4 | 1 | 0.1 | 19.97 | 0.84 | 0.23 to 2.09 | 0.39
  Littleton, NC | 4 | 1 | 0.1 | 15.81 | 0.84 | 0.23 to 2.09 | 0.39
  Hydaburg, AK | 56 | 1 | 0.1 | 9.10 | 0.83 | 0.23 to 2.08 | 0.39
  Stamford, CT | 8 | 1 | 0.1 | 7.25 | 0.83 | 0.23 to 2.07 | 0.39
top 8 by lower bound (most clearly over-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Mobile, AL | 1351 | 336 | 47.3 | 7.10 | 6.14 | 5.23 to 7.15 | 1.00
  Key West, FL | 51153 | 204 | 38.9 | 5.25 | 4.45 | 3.62 to 5.39 | 1.00
  Montgomery, AL | 135189 | 2058 | 646.8 | 3.18 | 3.15 | 2.95 to 3.35 | 1.00
  Birmingham, AL | 164346 | 3316 | 1207.4 | 2.75 | 2.73 | 2.60 to 2.87 | 1.00
  Huntsville, AL | 2796 | 201 | 73.7 | 2.73 | 2.52 | 2.05 to 3.06 | 1.00
  Monticello, AR | 4283 | 132 | 48.3 | 2.73 | 2.43 | 1.89 to 3.08 | 1.00
  Marysville, CA | 3999 | 298 | 155.4 | 1.92 | 1.86 | 1.57 to 2.18 | 1.00
  Batesville, AR | 18005 | 265 | 136.6 | 1.94 | 1.87 | 1.56 to 2.22 | 1.00
top 5 by upper bound (most clearly under-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  San Diego, CA | 8964 | 0 | 130.6 | 0.00 | 0.04 | 0.01 to 0.12 | 0.00
  Pittsburg, PA | 48710 | 3 | 68.2 | 0.04 | 0.12 | 0.04 to 0.27 | 0.00
  Flagstaff, AZ | 18728 | 53 | 218.8 | 0.24 | 0.26 | 0.17 to 0.37 | 0.00
  Skagway, AK | 24298 | 45 | 187.4 | 0.24 | 0.26 | 0.17 to 0.38 | 0.00
  Tampa, FL | 29059 | 1 | 36.9 | 0.03 | 0.16 | 0.04 to 0.39 | 0.00

## free_silver: "free silver"  1751-05-09 to 1963-12-31 by month (2552 buckets)
hits 34389  pages 6557925  places with pages 398  with hits 204  coverage cells 61770  scoring 8862 us
raw hits vs pages: spearman 0.736 (all places)  R^2 of log-log 0.409 (places with hits)
raw hits vs titles (whole corpus): spearman 0.341  R^2 of log-log 0.126
share-of-pages colour today: max 0.5000 at Laurel, DE (8 pages, 4 hits); 6 of 204 places with hits are in the top half of the scale
dispersion phi 4.65 from 94 places
prior: alpha 2.413  mean 0.982  (prior sd of lift 0.63)  scored with shape 2.385
90% interval above 1 / below 1 / includes 1: 27 / 32 / 339  (phi = 1: 48 / 70)
same search by year: phi 4.65 from 94 places, above/below 28 / 32, 3 places change flag
top 10 that are also among the 10 places with most pages: raw hits 7  share of pages 0  estimate 1  lower bound 2
top 10 by share of pages: 9 have fewer pages than the median place (1764)
without time standardization: 165 places change flag; largest gap Chicago, IL expected 9.8 with time vs 280.1 without (0.04x)
with the place in its own reference rate: 1 places change flag; Washington, DC raw lift 1.054 vs 1.069 against the others
top 5 by raw hits:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Washington, DC | 1990131 | 6595 | 6167.6 | 1.07 | 1.07 | 1.02 to 1.12 | 0.99
  New-York, NY | 699264 | 5579 | 5007.5 | 1.11 | 1.11 | 1.06 to 1.17 | 1.00
  San Francisco, CA | 208359 | 2556 | 3173.3 | 0.81 | 0.81 | 0.75 to 0.86 | 0.00
  Wilmington, DE | 192859 | 2082 | 1661.3 | 1.25 | 1.25 | 1.16 to 1.35 | 1.00
  Birmingham, AL | 164346 | 1975 | 1325.9 | 1.49 | 1.48 | 1.37 to 1.61 | 1.00
top 5 by raw lift:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Pinal City, AZ | 1129 | 1 | 0.1 | 15.62 | 0.93 | 0.25 to 2.33 | 0.46
  Columbus, OH | 8 | 1 | 0.1 | 13.18 | 0.93 | 0.25 to 2.33 | 0.46
  Seattle, WA | 8 | 2 | 0.2 | 9.97 | 1.01 | 0.29 to 2.44 | 0.50
  Augusta, GA | 80 | 1 | 0.1 | 9.52 | 0.93 | 0.25 to 2.32 | 0.45
  Parsons, KS | 4 | 1 | 0.1 | 9.52 | 0.93 | 0.25 to 2.32 | 0.45
top 8 by lower bound (most clearly over-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Florence, AZ | 1917 | 184 | 26.8 | 6.87 | 5.08 | 3.90 to 6.49 | 1.00
  Montgomery, AL | 135189 | 609 | 230.5 | 2.64 | 2.56 | 2.21 to 2.94 | 1.00
  Mineral Park, AZ | 9111 | 295 | 136.9 | 2.16 | 2.06 | 1.67 to 2.50 | 1.00
  Leavenworth, KS | 1261 | 96 | 40.5 | 2.37 | 2.04 | 1.41 to 2.82 | 1.00
  Smyrna, DE | 36878 | 252 | 136.4 | 1.85 | 1.77 | 1.41 to 2.19 | 1.00
  Prescott, AZ | 14966 | 246 | 132.9 | 1.85 | 1.77 | 1.41 to 2.19 | 1.00
  Birmingham, AL | 164346 | 1975 | 1325.9 | 1.49 | 1.48 | 1.37 to 1.61 | 1.00
  Topeka, KS | 7194 | 426 | 261.3 | 1.63 | 1.60 | 1.34 to 1.88 | 1.00
top 5 by upper bound (most clearly under-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Deland, FL | 9665 | 10 | 275.0 | 0.04 | 0.07 | 0.03 to 0.14 | 0.00
  San Diego, CA | 8964 | 0 | 111.6 | 0.00 | 0.08 | 0.02 to 0.20 | 0.00
  Little Rock, AR | 58416 | 29 | 287.4 | 0.10 | 0.13 | 0.07 to 0.22 | 0.00
  Newtown, CT | 11767 | 19 | 218.3 | 0.09 | 0.12 | 0.06 to 0.23 | 0.00
  Ocala, FL | 15152 | 79 | 336.1 | 0.24 | 0.25 | 0.17 to 0.36 | 0.00

## cross_of_gold: "cross of gold"  1896-06-01 to 1896-12-31 by week (31 buckets)
hits 263  pages 39423  places with pages 58  with hits 32  coverage cells 1481  scoring 809 us
raw hits vs pages: spearman 0.597 (all places)  R^2 of log-log 0.660 (places with hits)
raw hits vs titles (whole corpus): spearman 0.555  R^2 of log-log 0.394
share-of-pages colour today: max 0.2500 at Laurel, DE (4 pages, 1 hits); 1 of 32 places with hits are in the top half of the scale
dispersion phi 2.33 from 5 places
prior: alpha 10.129  mean 0.995  (prior sd of lift 0.31)  scored with shape 8.663
90% interval above 1 / below 1 / includes 1: 0 / 1 / 57  (phi = 1: 4 / 6)
top 10 that are also among the 10 places with most pages: raw hits 9  share of pages 0  estimate 5  lower bound 5
top 10 by share of pages: 7 have fewer pages than the median place (128)
without time standardization: 1 places change flag; largest gap Tombstone, AZ expected 5.7 with time vs 4.8 without (1.18x)
with the place in its own reference rate: 1 places change flag; New-York, NY raw lift 1.219 vs 1.276 against the others
top 5 by raw hits:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  New-York, NY | 7417 | 58 | 45.4 | 1.28 | 1.18 | 0.87 to 1.55 | 0.82
  San Francisco, CA | 3641 | 31 | 23.9 | 1.30 | 1.14 | 0.78 to 1.59 | 0.73
  Washington, DC | 7011 | 30 | 49.4 | 0.61 | 0.71 | 0.49 to 0.99 | 0.05
  Los Angeles, CA | 2440 | 27 | 15.7 | 1.72 | 1.29 | 0.87 to 1.82 | 0.86
  Birmingham, AL | 1638 | 23 | 10.2 | 2.25 | 1.39 | 0.92 to 2.00 | 0.91
top 5 by raw lift:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Laurel, DE | 4 | 1 | 0.0 | 62.55 | 1.01 | 0.55 to 1.67 | 0.51
  Prescott, AZ | 118 | 5 | 0.7 | 7.00 | 1.16 | 0.67 to 1.85 | 0.68
  Wichita, KS | 90 | 3 | 0.7 | 4.28 | 1.07 | 0.60 to 1.74 | 0.58
  Cleveland, OH | 134 | 3 | 0.9 | 3.19 | 1.06 | 0.59 to 1.72 | 0.57
  Meeker, CO | 116 | 2 | 0.8 | 2.53 | 1.02 | 0.56 to 1.67 | 0.52
top 8 by lower bound (most clearly over-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Birmingham, AL | 1638 | 23 | 10.2 | 2.25 | 1.39 | 0.92 to 2.00 | 0.91
  New-York, NY | 7417 | 58 | 45.4 | 1.28 | 1.18 | 0.87 to 1.55 | 0.82
  Los Angeles, CA | 2440 | 27 | 15.7 | 1.72 | 1.29 | 0.87 to 1.82 | 0.86
  San Francisco, CA | 3641 | 31 | 23.9 | 1.30 | 1.14 | 0.78 to 1.59 | 0.73
  New Haven, CT | 1500 | 15 | 9.7 | 1.55 | 1.15 | 0.73 to 1.71 | 0.70
  Prescott, AZ | 118 | 5 | 0.7 | 7.00 | 1.16 | 0.67 to 1.85 | 0.68
  Texarkana, AR | 612 | 6 | 3.9 | 1.53 | 1.05 | 0.61 to 1.66 | 0.56
  Flagstaff, AZ | 224 | 4 | 1.7 | 2.30 | 1.06 | 0.60 to 1.71 | 0.58
top 5 by upper bound (most clearly under-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Washington, DC | 7011 | 30 | 49.4 | 0.61 | 0.71 | 0.49 to 0.99 | 0.05
  Waterbury, CT | 1443 | 4 | 9.9 | 0.40 | 0.77 | 0.44 to 1.25 | 0.20
  Tombstone, AZ | 726 | 1 | 5.7 | 0.17 | 0.78 | 0.43 to 1.30 | 0.23
  Phoenix, AZ | 1460 | 6 | 10.1 | 0.59 | 0.84 | 0.49 to 1.32 | 0.27
  Ocala, FL | 656 | 0 | 4.2 | 0.00 | 0.79 | 0.42 to 1.33 | 0.24

## boll_weevil: "boll weevil"  1895-01-01 to 1930-12-31 by year (36 buckets)
hits 21782  pages 3699444  places with pages 262  with hits 164  coverage cells 3326  scoring 4016 us
raw hits vs pages: spearman 0.759 (all places)  R^2 of log-log 0.326 (places with hits)
raw hits vs titles (whole corpus): spearman 0.165  R^2 of log-log 0.059
share-of-pages colour today: max 0.1250 at Columbia, SC (8 pages, 1 hits); 1 of 164 places with hits are in the top half of the scale
dispersion phi 4.85 from 76 places
prior: alpha 0.519  mean 1.087  (prior sd of lift 1.51)  scored with shape 0.510
90% interval above 1 / below 1 / includes 1: 34 / 104 / 124  (phi = 1: 41 / 130)
top 10 that are also among the 10 places with most pages: raw hits 4  share of pages 1  estimate 2  lower bound 2
top 10 by share of pages: 4 have fewer pages than the median place (3417)
without time standardization: 22 places change flag; largest gap Deland, FL expected 7.0 with time vs 28.2 without (0.25x)
with the place in its own reference rate: 0 places change flag; Washington, DC raw lift 0.360 vs 0.284 against the others
top 5 by raw hits:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Montgomery, AL | 135189 | 6709 | 706.8 | 9.49 | 9.46 | 9.05 to 9.89 | 1.00
  Birmingham, AL | 162914 | 4396 | 903.8 | 4.86 | 4.85 | 4.59 to 5.12 | 1.00
  Washington, DC | 841532 | 1867 | 6580.1 | 0.28 | 0.28 | 0.26 to 0.31 | 0.00
  Pine Bluff, AR | 64596 | 792 | 315.5 | 2.51 | 2.49 | 2.19 to 2.83 | 1.00
  New-York, NY | 325934 | 527 | 1871.2 | 0.28 | 0.28 | 0.24 to 0.33 | 0.00
top 5 by raw lift:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Magnolia, AR | 2392 | 144 | 12.9 | 11.18 | 9.55 | 6.96 to 12.72 | 1.00
  Montgomery, AL | 135189 | 6709 | 706.8 | 9.49 | 9.46 | 9.05 to 9.89 | 1.00
  Pulaski Heights, AR | 2534 | 139 | 17.8 | 7.83 | 6.98 | 5.06 to 9.34 | 1.00
  Marianna, AR | 6356 | 342 | 43.9 | 7.79 | 7.42 | 6.06 to 8.97 | 1.00
  Osceola, AR | 4955 | 220 | 30.0 | 7.34 | 6.85 | 5.31 to 8.66 | 1.00
top 8 by lower bound (most clearly over-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Montgomery, AL | 135189 | 6709 | 706.8 | 9.49 | 9.46 | 9.05 to 9.89 | 1.00
  Magnolia, AR | 2392 | 144 | 12.9 | 11.18 | 9.55 | 6.96 to 12.72 | 1.00
  Marianna, AR | 6356 | 342 | 43.9 | 7.79 | 7.42 | 6.06 to 8.97 | 1.00
  Osceola, AR | 4955 | 220 | 30.0 | 7.34 | 6.85 | 5.31 to 8.66 | 1.00
  Pulaski Heights, AR | 2534 | 139 | 17.8 | 7.83 | 6.98 | 5.06 to 9.34 | 1.00
  Monticello, AR | 4223 | 163 | 22.3 | 7.32 | 6.68 | 4.96 to 8.75 | 1.00
  Ashdown, AR | 3419 | 174 | 26.1 | 6.67 | 6.17 | 4.63 to 8.01 | 1.00
  Birmingham, AL | 162914 | 4396 | 903.8 | 4.86 | 4.85 | 4.59 to 5.12 | 1.00
top 5 by upper bound (most clearly under-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Skagway, AK | 24298 | 0 | 145.8 | 0.00 | 0.01 | 0.00 to 0.06 | 0.00
  Pittsburg, PA | 16100 | 0 | 109.3 | 0.00 | 0.01 | 0.00 to 0.08 | 0.00
  Tucson, AZ | 20181 | 2 | 121.8 | 0.02 | 0.02 | 0.00 to 0.11 | 0.00
  Nome, AK | 22164 | 6 | 158.6 | 0.04 | 0.04 | 0.01 to 0.13 | 0.00
  Valdez, AK | 9457 | 0 | 60.0 | 0.00 | 0.02 | 0.00 to 0.15 | 0.00

## klondike: klondike  1896-01-01 to 1901-12-31 by month (72 buckets)
hits 22132  pages 437180  places with pages 163  with hits 120  coverage cells 5107  scoring 2514 us
raw hits vs pages: spearman 0.883 (all places)  R^2 of log-log 0.779 (places with hits)
raw hits vs titles (whole corpus): spearman 0.156  R^2 of log-log 0.030
share-of-pages colour today: max 0.7500 at Batesville, AR (4 pages, 3 hits); 6 of 120 places with hits are in the top half of the scale
dispersion phi 3.94 from 67 places
prior: alpha 1.229  mean 1.401  (prior sd of lift 1.26)  scored with shape 1.207
90% interval above 1 / below 1 / includes 1: 28 / 28 / 107  (phi = 1: 38 / 44)
same search by year: phi 3.94 from 67 places, above/below 28 / 28, 2 places change flag
top 10 that are also among the 10 places with most pages: raw hits 8  share of pages 0  estimate 0  lower bound 1
top 10 by share of pages: 7 have fewer pages than the median place (463)
without time standardization: 17 places change flag; largest gap Milford, DE expected 5.1 with time vs 18.6 without (0.27x)
with the place in its own reference rate: 0 places change flag; Washington, DC raw lift 0.875 vs 0.847 against the others
top 5 by raw hits:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  San Francisco, CA | 39426 | 3157 | 2007.6 | 1.57 | 1.57 | 1.48 to 1.66 | 1.00
  Washington, DC | 78127 | 3144 | 3710.3 | 0.85 | 0.85 | 0.80 to 0.90 | 0.00
  New-York, NY | 67799 | 2838 | 3672.9 | 0.77 | 0.77 | 0.73 to 0.82 | 0.00
  Los Angeles, CA | 13577 | 1344 | 889.9 | 1.51 | 1.51 | 1.38 to 1.65 | 1.00
  Skagway, AK | 1900 | 1109 | 46.0 | 24.11 | 22.52 | 20.39 to 24.80 | 1.00
top 5 by raw lift:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Skagway, AK | 1900 | 1109 | 46.0 | 24.11 | 22.52 | 20.39 to 24.80 | 1.00
  Douglas City, AK | 668 | 249 | 22.4 | 11.13 | 9.79 | 7.92 to 11.95 | 1.00
  Eagle City, AK | 2 | 1 | 0.1 | 9.60 | 1.29 | 0.18 to 4.32 | 0.61
  Seattle, WA | 8 | 3 | 0.4 | 8.38 | 1.73 | 0.36 to 4.93 | 0.74
  Skaguay Alaska, AK | 54 | 36 | 4.8 | 7.51 | 4.82 | 2.73 to 7.76 | 1.00
top 8 by lower bound (most clearly over-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Skagway, AK | 1900 | 1109 | 46.0 | 24.11 | 22.52 | 20.39 to 24.80 | 1.00
  Douglas City, AK | 668 | 249 | 22.4 | 11.13 | 9.79 | 7.92 to 11.95 | 1.00
  Fort Wrangel, AK | 476 | 209 | 31.0 | 6.74 | 6.17 | 4.89 to 7.66 | 1.00
  Skaguay Alaska, AK | 54 | 36 | 4.8 | 7.51 | 4.82 | 2.73 to 7.76 | 1.00
  Arizona City, AZ | 1188 | 141 | 48.7 | 2.90 | 2.78 | 2.09 to 3.60 | 1.00
  Mineral Park, AZ | 1561 | 168 | 70.1 | 2.40 | 2.33 | 1.80 to 2.96 | 1.00
  Salisbury, CT | 1297 | 147 | 64.3 | 2.29 | 2.22 | 1.68 to 2.87 | 1.00
  Elbert, CO | 463 | 66 | 26.3 | 2.51 | 2.34 | 1.54 to 3.38 | 1.00
top 5 by upper bound (most clearly under-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  San Diego, CA | 1242 | 0 | 60.4 | 0.00 | 0.06 | 0.01 to 0.21 | 0.00
  Phoenix, AZ | 13908 | 160 | 771.3 | 0.21 | 0.21 | 0.16 to 0.27 | 0.00
  Oklahoma City, OK | 1082 | 0 | 37.2 | 0.00 | 0.09 | 0.01 to 0.33 | 0.00
  Arizola, AZ | 3081 | 33 | 155.7 | 0.21 | 0.23 | 0.13 to 0.38 | 0.00
  Florence, CO | 3622 | 42 | 168.5 | 0.25 | 0.26 | 0.16 to 0.41 | 0.00

## mormon: mormon  1850-01-01 to 1930-12-31 by year (81 buckets)
hits 59718  pages 4623735  places with pages 336  with hits 261  coverage cells 4773  scoring 4355 us
raw hits vs pages: spearman 0.862 (all places)  R^2 of log-log 0.640 (places with hits)
raw hits vs titles (whole corpus): spearman 0.359  R^2 of log-log 0.146
share-of-pages colour today: max 0.5455 at Mariposa, CA (209 pages, 114 hits); 2 of 261 places with hits are in the top half of the scale
dispersion phi 3.29 from 153 places
prior: alpha 1.407  mean 1.172  (prior sd of lift 0.99)  scored with shape 1.396
90% interval above 1 / below 1 / includes 1: 53 / 86 / 197  (phi = 1: 67 / 125)
top 10 that are also among the 10 places with most pages: raw hits 6  share of pages 0  estimate 0  lower bound 0
top 10 by share of pages: 10 have fewer pages than the median place (2124)
without time standardization: 76 places change flag; largest gap Mariposa, CA expected 19.5 with time vs 2.7 without (7.21x)
with the place in its own reference rate: 1 places change flag; Washington, DC raw lift 0.790 vs 0.736 against the others
top 5 by raw hits:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  New-York, NY | 583771 | 9582 | 11910.7 | 0.80 | 0.80 | 0.78 to 0.83 | 0.00
  Washington, DC | 989067 | 8813 | 11976.5 | 0.74 | 0.74 | 0.71 to 0.76 | 0.00
  Phoenix, AZ | 121420 | 2953 | 1113.8 | 2.65 | 2.65 | 2.50 to 2.79 | 1.00
  San Francisco, CA | 208359 | 2778 | 2325.5 | 1.19 | 1.19 | 1.13 to 1.26 | 1.00
  Chicago, IL | 53408 | 2279 | 1377.2 | 1.65 | 1.65 | 1.55 to 1.76 | 1.00
top 5 by raw lift:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Parsons, KS | 4 | 2 | 0.1 | 31.47 | 1.39 | 0.29 to 3.92 | 0.66
  Xenia, OH | 4 | 1 | 0.1 | 15.72 | 1.14 | 0.20 to 3.51 | 0.56
  Corsicana, TX | 4 | 1 | 0.1 | 15.72 | 1.14 | 0.20 to 3.51 | 0.56
  Macon, GA | 4 | 1 | 0.1 | 15.72 | 1.14 | 0.20 to 3.51 | 0.56
  Snowflake, AZ | 2398 | 151 | 12.7 | 11.89 | 9.29 | 7.24 to 11.70 | 1.00
top 8 by lower bound (most clearly over-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Flagstaff, AZ | 18728 | 1880 | 167.7 | 11.21 | 10.97 | 10.24 to 11.75 | 1.00
  Safford, AZ | 5635 | 518 | 51.7 | 10.02 | 9.37 | 8.20 to 10.65 | 1.00
  Snowflake, AZ | 2398 | 151 | 12.7 | 11.89 | 9.29 | 7.24 to 11.70 | 1.00
  Peach Springs, AZ | 1762 | 389 | 65.9 | 5.90 | 5.62 | 4.82 to 6.51 | 1.00
  Mariposa, CA | 209 | 114 | 19.5 | 5.86 | 5.03 | 3.77 to 6.54 | 1.00
  St. Johns, AZ | 10869 | 482 | 125.6 | 3.84 | 3.75 | 3.26 to 4.28 | 1.00
  Douglas, AZ | 25145 | 477 | 125.8 | 3.79 | 3.71 | 3.22 to 4.23 | 1.00
  Globe, AZ | 7554 | 201 | 51.6 | 3.89 | 3.68 | 2.97 to 4.51 | 1.00
top 5 by upper bound (most clearly under-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  San Diego, CA | 8964 | 1 | 123.2 | 0.01 | 0.04 | 0.01 to 0.11 | 0.00
  Pittsburg, PA | 16100 | 0 | 90.6 | 0.00 | 0.04 | 0.01 to 0.13 | 0.00
  Deland, FL | 9665 | 11 | 215.2 | 0.05 | 0.07 | 0.03 to 0.13 | 0.00
  Texarkana, AR | 9569 | 16 | 129.3 | 0.12 | 0.15 | 0.07 to 0.27 | 0.00
  Newtown, CT | 11767 | 44 | 183.9 | 0.24 | 0.25 | 0.16 to 0.38 | 0.00

## baking_powder: "baking powder"  1880-01-01 to 1920-12-31 by year (41 buckets)
hits 111479  pages 3190497  places with pages 282  with hits 209  coverage cells 3466  scoring 5175 us
raw hits vs pages: spearman 0.916 (all places)  R^2 of log-log 0.767 (places with hits)
raw hits vs titles (whole corpus): spearman 0.302  R^2 of log-log 0.085
share-of-pages colour today: max 0.2628 at Bessemer, CO (392 pages, 103 hits); 5 of 209 places with hits are in the top half of the scale
dispersion phi 11.62 from 155 places
prior: alpha 2.546  mean 1.204  (prior sd of lift 0.75)  scored with shape 2.524
90% interval above 1 / below 1 / includes 1: 63 / 33 / 186  (phi = 1: 102 / 88)
top 10 that are also among the 10 places with most pages: raw hits 8  share of pages 0  estimate 0  lower bound 0
top 10 by share of pages: 8 have fewer pages than the median place (2041)
without time standardization: 44 places change flag; largest gap Hardy, AR expected 17.3 with time vs 4.8 without (3.65x)
with the place in its own reference rate: 0 places change flag; Washington, DC raw lift 0.625 vs 0.581 against the others
top 5 by raw hits:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Washington, DC | 562296 | 11954 | 20573.7 | 0.58 | 0.58 | 0.55 to 0.61 | 0.00
  New Haven, CT | 108487 | 7219 | 3982.2 | 1.81 | 1.81 | 1.69 to 1.93 | 1.00
  New-York, NY | 322129 | 6209 | 13541.9 | 0.46 | 0.46 | 0.43 to 0.49 | 0.00
  Los Angeles, CA | 97955 | 4919 | 4813.2 | 1.02 | 1.02 | 0.94 to 1.11 | 0.67
  Sacramento, CA | 39799 | 4782 | 2323.0 | 2.06 | 2.05 | 1.89 to 2.22 | 1.00
top 5 by raw lift:
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Laurel, DE | 8 | 2 | 0.3 | 6.78 | 1.12 | 0.31 to 2.75 | 0.57
  Wilmington, NC | 4 | 1 | 0.1 | 6.74 | 1.08 | 0.30 to 2.70 | 0.55
  Oakland, CA | 104 | 12 | 2.6 | 4.57 | 1.39 | 0.48 to 3.06 | 0.71
  Portland, OR | 416 | 46 | 11.2 | 4.10 | 2.01 | 0.96 to 3.64 | 0.94
  Williams, AZ | 5122 | 474 | 132.1 | 3.59 | 3.19 | 2.46 to 4.06 | 1.00
top 8 by lower bound (most clearly over-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  Williams, AZ | 5122 | 474 | 132.1 | 3.59 | 3.19 | 2.46 to 4.06 | 1.00
  Elbert, CO | 7989 | 682 | 212.8 | 3.20 | 2.98 | 2.40 to 3.66 | 1.00
  Irwin, CO | 4626 | 430 | 124.5 | 3.45 | 3.06 | 2.32 to 3.93 | 1.00
  Putnam, CT | 11934 | 832 | 283.1 | 2.94 | 2.79 | 2.29 to 3.36 | 1.00
  Willcox, AZ | 6717 | 738 | 247.0 | 2.99 | 2.81 | 2.28 to 3.42 | 1.00
  Waterbury, CT | 46475 | 4500 | 2065.8 | 2.18 | 2.17 | 1.99 to 2.35 | 1.00
  Cañon City, CO | 6022 | 486 | 178.5 | 2.72 | 2.52 | 1.95 to 3.20 | 1.00
  Sacramento, CA | 39799 | 4782 | 2323.0 | 2.06 | 2.05 | 1.89 to 2.22 | 1.00
top 5 by upper bound (most clearly under-represented):
  place | pages | hits | expected | raw lift | estimate | 90% interval | P(above 1)
  San Diego, CA | 8152 | 0 | 362.2 | 0.00 | 0.07 | 0.02 to 0.17 | 0.00
  Cordova, AK | 10564 | 21 | 225.8 | 0.09 | 0.19 | 0.07 to 0.38 | 0.00
  Indianapolis, IN | 12655 | 140 | 584.3 | 0.24 | 0.27 | 0.17 to 0.41 | 0.00
  Skagway, AK | 23056 | 155 | 628.4 | 0.25 | 0.28 | 0.18 to 0.41 | 0.00
  Washington City, DC | 16745 | 227 | 817.3 | 0.28 | 0.30 | 0.21 to 0.42 | 0.00

## Timing
3,000 places with no difference between them: alpha 1000000, scoring 49 ms
```

</details>
