# Research: 100 additional historical search examples

This is the record of the original additions, not a specification of today's
presets. The subsequent [full-catalog audit][audit] corrects historical wording,
date windows and filters, and adds Relative rate and States examples. Current
definitions are in `web/src/examples.json`; the audit records the reasons,
sources and newer corpus observations without rewriting the original ledger.

[audit]: https://github.com/tgoodyear/usnewsmap.com/issues/1360

## Scope and method

Added exactly **100** hand-authored presets to the existing 100 in
`web/src/examples.json`, preserving every original entry and the shared schema.
These are curated research queries, not visitor queries or search-log extracts.
No production settings, services, or warm-up budgets were changed.

The additions span **1754–1963**: all seven existing eras receive 13–15 new
examples, and five colonial/early-republic examples add a `1751-1828` rotation
group. Live `/v1/meta` reported overall corpus bounds of 1736-09-03 through
1963-12-31; the selection does not imply continuous coverage of every year.

| Era       | Added |
| --------- | ----: |
| 1751-1828 |     5 |
| 1828-1860 |    13 |
| 1860-1865 |    13 |
| 1865-1877 |    13 |
| 1877-1900 |    15 |
| 1900-1929 |    14 |
| 1929-1945 |    13 |
| 1945-1963 |    14 |

Selection began with the [Library of Congress topic-guide directory][directory],
then the individual introductions, timelines, and suggested period vocabulary.
The table cites **98 distinct institutional resources**, including
Smithsonian collections, National Park Service histories and landmark nominations,
National Archives documents, Massachusetts Historical Society primary sources,
Alabama's trial records, NOAA, the Royal Geographical Society, the Antarctic
Treaty Secretariat, and the National Baseball Hall of Fame.

The new topics were compared with the original 100 titles, IDs, and expressions,
not merely renamed or given different date windows. The additions emphasize
agriculture, manufacturing, medicine, education, labor, Indigenous history,
Black cultural life, women performers and aviators, recreation, language,
environment, regional infrastructure, and international science.

## Live verification

- Checked **2026-10-27T00:37:30.020479+00:00 through 2026-10-27T00:53:38.848954+00:00** (UTC), against
  `pages-v20261022-1`; every accepted aggregate returned HTTP 200, nonzero
  matching pages, and nonzero places.
- Used the exact saved `aggregate` strings, including `mode=phrase` for plain
  words. Quoted phrases, Boolean expressions, and prefixes use query syntax
  without a mode override, matching the web client's behavior.
- Aggregate requests used at most **two concurrent workers** and narrow historical
  windows. The validation client was prepared to poll 202 responses and wait on
  429/502/503/504 responses, honoring `Retry-After` with bounded delays and a
  three-minute deadline. All accepted requests completed on their first attempt.
- Read the aggregate's first/last snippets for every candidate. Where homonyms,
  OCR, or whole-page co-occurrence made these inconclusive, inspected up to five
  relevance-ranked `/v1/hits` results from a matching place, also at concurrency
  two. The primary-source links below identify an inspected matching page;
  the adjacent description is a paraphrase, not an OCR quotation.
- Rejected zero-result wording such as `mechanical reaper` in 1851–1853 in
  favor of the attested `reaping machine`. Replaced the initial Oneida search
  with Brook Farm. A cotton-gin candidate was rejected because snippets instead
  joined cotton and alcoholic gin in advertisements. `cow pox`/`cow pock`
  supplied an independently researched early medical topic instead.
- Disambiguated Darwin with `Origin of Species`, Major Taylor with `bicycle`,
  Esperanto with `language`, and Bessie Coleman with `aviat*`.
  `Tuskegee AND pilot*` proved too broad because it also matched sports teams
  “piloted” by coaches; the accepted expression uses `"fighter pilots"`.
  `braceros` retrieves actual Spanish-language labor reporting rather than
  singular-word matches about household braziers.

The linked aggregate counts are **matching newspaper pages, not articles or
confirmed accounts of the event**. Whole-page Boolean matches, repeated
advertisements, historical prejudice, inaccurate contemporary reporting, and
imperfect OCR remain possible. Institutional histories provide context rather
than endorsing the newspapers' claims or terminology. For example, the Tulsa
query follows period wording while the title correctly identifies a massacre.
The Yosemite search also includes scenery and travel coverage, not only the grant.

Colonial coverage is deliberately transparent: the Albany search has only two
pages from one place, including the May 9, 1754 Pennsylvania Gazette page
associated with Franklin's “Join or Die” cartoon. Stamp Act coverage has two
places. Later cultural topics can also be sparse (Tallchief: three places).
These are meaningful primary sources, not evidence of comprehensive geography.
Counts are a dated observation, not a future availability or performance promise.

## Query and source ledger

Each pages/places link replays the exact aggregate request. Each dated newspaper
link opens a sampled primary page. Source links supply institutional context;
the example's title and blurb remain in `examples.json`.

### 1828-1860

| ID                   | Query                  | Window                   |   Pages / places | Inspected primary evidence                                                                             | Context           |
| -------------------- | ---------------------- | ------------------------ | ---------------: | ------------------------------------------------------------------------------------------------------ | ----------------- |
| `daguerreotype`      | `daguerreotype`        | 1839-01-01 to 1841-12-31 |   [462 / 39][A1] | [1839-09-25, Daily National Intelligencer][P1]: Explains Daguerre's photographic invention.            | [Smithsonian][S1] |
| `mechanical-reaper`  | `reaping machine`      | 1851-01-01 to 1853-12-31 |   [438 / 76][A2] | [1851-02-22, The Cecil Whig][P2]: Offers a Hussey harvesting machine for sale.                         | [Smithsonian][S2] |
| `sewing-machine`     | `sewing machine`       | 1851-01-01 to 1853-12-31 | [1,933 / 83][A3] | [1851-01-01, New-York Daily Tribune][P3]: Advertises a stitch-forming sewing machine.                  | [Smithsonian][S2] |
| `vulcanized-rubber`  | `Goodyear AND rubber`  | 1845-01-01 to 1847-12-31 |   [921 / 32][A4] | [1847-12-31, New-York Daily Tribune][P4]: Advertises Goodyear patent overshoes and rubber goods.       | [Smithsonian][S2] |
| `electric-telegraph` | `electric telegraph`   | 1844-01-01 to 1846-12-31 |   [436 / 57][A5] | [1844-02-07, Danbury Times][P5]: Reports work on Morse's Washington-to-Baltimore line.                 | [LoC][S3]         |
| `asylum-reform`      | `insane asylum`        | 1843-01-01 to 1845-12-31 |   [376 / 52][A6] | [1843-01-02, The New York Herald][P6]: Discusses an asylum application to the legislature.             | [LoC][S4]         |
| `dorr-rebellion`     | `"Dorr" AND rebellion` | 1842-01-01 to 1843-12-31 |   [338 / 55][A7] | [1842-05-21, The Madisonian][P7]: Debates Rhode Island's constitution and Dorr's party.                | [LoC][S5]         |
| `bloomer-dress`      | `Bloomer costume`      | 1851-01-01 to 1853-12-31 |   [650 / 98][A8] | [1851-05-31, The Caledonian][P8]: Reports the new costume appearing in Vermont.                        | [LoC][S6]         |
| `darwin-origin`      | `Origin of Species`    | 1859-01-01 to 1860-12-31 |   [235 / 57][A9] | [1860-08-06, Daily National Intelligencer][P9]: Discusses Darwin's theory at the British Association.  | [LoC][S7]         |
| `leaves-of-grass`    | `Leaves of Grass`      | 1855-01-01 to 1857-12-31 |   [121 / 7][A10] | [1855-07-09, New-York Daily Tribune][P10]: Advertises Whitman's new poetry volume.                     | [LoC][S8]         |
| `brook-farm`         | `Brook Farm`           | 1841-01-01 to 1845-12-31 |   [97 / 14][A11] | [1842-08-13, New-York Daily Tribune][P11]: Publishes news from the communal experiment.                | [LoC][S9]         |
| `chang-and-eng`      | `Chang and Eng`        | 1830-01-01 to 1832-12-31 |    [13 / 9][A12] | [1831-08-09, Daily National Intelligencer][P12]: Reports a legal case involving the touring brothers.  | [LoC][S10]        |
| `circus-menageries`  | `menagerie`            | 1840-01-01 to 1842-12-31 |  [447 / 74][A13] | [1842-12-23, Daily National Intelligencer][P13]: Describes an incident while moving exhibition tigers. | [LoC][S11]        |

### 1860-1865

| ID                     | Query                                   | Window                   |     Pages / places | Inspected primary evidence                                                                                   | Context                  |
| ---------------------- | --------------------------------------- | ------------------------ | -----------------: | ------------------------------------------------------------------------------------------------------------ | ------------------------ |
| `civil-war-ballooning` | `balloon corps`                         | 1861-01-01 to 1863-12-31 |     [55 / 35][A14] | [1861-10-18, Cincinnati Daily Press][P14]: Reports Lowe organizing military balloon reconnaissance.          | [LoC][S12]               |
| `blockade-runners`     | `blockade runner`                       | 1863-01-01 to 1864-12-31 | [3,022 / 151][A15] | [1863-01-13, New-York Daily Tribune][P15]: Reports a blockade-running steamer.                               | [LoC][S13]               |
| `clara-barton`         | `Clara Barton`                          | 1863-01-01 to 1865-12-31 |    [152 / 60][A16] | [1863-07-21, Worcester Daily Spy][P16]: Prints correspondence to a soldiers' relief committee.               | [NPS][S14]               |
| `sultana-disaster`     | `Sultana AND explosion`                 | 1865-04-27 to 1865-07-31 |    [147 / 68][A17] | [1865-04-29, Evening Star][P17]: Reports the Mississippi steamboat explosion and casualties.                 | [LoC][S15]               |
| `belle-boyd`           | `Belle Boyd`                            | 1862-01-01 to 1864-12-31 |    [315 / 81][A18] | [1862-05-31, Evening Star][P18]: Discusses Boyd's wartime movements in Virginia.                             | [LoC][S16]               |
| `sanitary-commission`  | `sanitary commission`                   | 1861-01-01 to 1863-12-31 | [5,212 / 156][A19] | [1861-05-21, Daily National Intelligencer][P19]: Proposes organized medical and sanitary support.            | [NPS][S17]               |
| `edwin-booth`          | `Edwin Booth`                           | 1863-01-01 to 1864-12-31 |    [411 / 25][A20] | [1863-01-09, The Daily Manchester American][P20]: Discusses a sculpture portraying Booth as Hamlet.          | [LoC][S18]               |
| `homestead-act`        | `homestead act`                         | 1862-01-01 to 1863-12-31 |    [256 / 63][A21] | [1862-05-02, Chicago Daily Tribune][P21]: Debates homestead legislation and military eligibility.            | [National Archives][S19] |
| `morrill-act`          | `agricultural colleges`                 | 1862-01-01 to 1863-12-31 |    [239 / 67][A22] | [1863-12-31, Gold Hill Daily News][P22]: Explains land warrants supporting agricultural colleges.            | [National Archives][S20] |
| `draft-riots`          | `"New York" AND "draft riots"`          | 1863-07-13 to 1863-09-30 |     [37 / 20][A23] | [1863-09-26, The Daily Green Mountain Freeman][P23]: Discusses the New York riots and military consequences. | [NPS][S21]               |
| `port-royal-schools`   | `"Port Royal" AND freedmen AND school*` | 1862-01-01 to 1864-12-31 |    [396 / 62][A24] | [1863-08-06, New-York Daily Tribune][P24]: Reports freedpeople's schools and testimony from Port Royal.      | [NPS][S22]               |
| `sand-creek`           | `"Sand Creek" AND massacre`             | 1864-11-29 to 1865-12-31 |     [38 / 18][A25] | [1865-12-30, The Independent][P25]: Discusses the massacre and Chivington's reputation.                      | [NPS][S23]               |
| `yosemite-grant`       | `Yosemite valley`                       | 1864-01-01 to 1865-12-31 |     [65 / 26][A26] | [1865-12-30, The Mountain Democrat][P26]: Advertises images of the protected valley's scenery.               | [LoC][S24]               |

### 1865-1877

| ID                         | Query                            | Window                   |     Pages / places | Inspected primary evidence                                                                         | Context                          |
| -------------------------- | -------------------------------- | ------------------------ | -----------------: | -------------------------------------------------------------------------------------------------- | -------------------------------- |
| `alaska-purchase`          | `Russian America`                | 1867-01-01 to 1868-12-31 | [1,715 / 171][A27] | [1868-12-31, Democratic Enquirer][P27]: Lists the purchase expenditure for Russian America.        | [LoC][S25]                       |
| `fenian-raids`             | `Fenian AND Canada`              | 1866-05-01 to 1866-07-31 | [2,168 / 164][A28] | [1866-07-31, The Potter Journal][P28]: Discusses Fenian prisoners held in Canada.                  | [LoC][S26]                       |
| `brooklyn-theater-fire`    | `"Brooklyn" AND "theatre fire"`  | 1876-12-05 to 1877-02-28 |     [91 / 34][A29] | [1876-12-07, The New York Herald][P29]: Reports the theater disaster and recovery of victims.      | [LoC][S27]                       |
| `yellowstone-park`         | `Yellowstone park`               | 1872-01-01 to 1873-12-31 |     [93 / 41][A30] | [1872-01-24, The New York Herald][P30]: Reports Senate discussion of the proposed park.            | [LoC][S28]                       |
| `navajo-treaty`            | `Navajo AND treaty`              | 1868-01-01 to 1869-12-31 |    [130 / 35][A31] | [1868-11-20, The New York Herald][P31]: Reports the Fort Sumner treaty concluded in June 1868.     | [National Archives][S29]         |
| `telephone-invention`      | `telephone`                      | 1876-01-01 to 1877-12-31 | [2,903 / 234][A32] | [1876-01-26, Watertown Republican][P32]: Discusses Gray's telephone and electrical experiments.    | [LoC][S30]                       |
| `red-stockings`            | `"Red Stockings" AND Cincinnati` | 1869-01-01 to 1870-12-31 |  [1,240 / 88][A33] | [1870-10-17, The Sun][P33]: Reports Cincinnati's defeat in a Chicago baseball game.                | [Baseball Hall of Fame][S31]     |
| `typewriter`               | `type writer`                    | 1873-01-01 to 1875-12-31 |    [451 / 56][A34] | [1873-02-10, Chicago Daily Tribune][P34]: Offers document copying with a writing machine.          | [Smithsonian][S32]               |
| `brooklyn-bridge-building` | `East river bridge`              | 1870-01-01 to 1872-12-31 |    [983 / 94][A35] | [1872-12-29, The New York Herald][P35]: Reports appraisal of land for the bridge anchorage.        | [LoC][S33]                       |
| `molly-maguires`           | `Molly Maguires`                 | 1875-01-01 to 1877-12-31 | [1,353 / 191][A36] | [1877-12-31, New-York Tribune][P36]: Reports an arrest connected to the coal-region prosecutions.  | [LoC][S34]                       |
| `grasshopper-plague`       | `grasshoppers`                   | 1874-01-01 to 1875-12-31 | [9,424 / 237][A37] | [1875-12-31, Bozeman Avant Courier][P37]: Describes crop destruction and regional food dependence. | [Kansas Historical Society][S35] |
| `challenger-expedition`    | `Challenger expedition`          | 1873-01-01 to 1876-12-31 |     [74 / 38][A38] | [1873-02-13, New-York Tribune][P38]: Discusses Thomson's leadership of the scientific expedition.  | [NOAA][S36]                      |
| `suez-canal-opening`       | `Suez canal`                     | 1869-01-01 to 1870-12-31 | [3,529 / 182][A39] | [1869-01-01, The New Orleans Crescent][P39]: Anticipates completion of the canal during 1869.      | [Smithsonian][S37]               |

### 1877-1900

| ID                   | Query                          | Window                   |      Pages / places | Inspected primary evidence                                                                                | Context    |
| -------------------- | ------------------------------ | ------------------------ | ------------------: | --------------------------------------------------------------------------------------------------------- | ---------- |
| `bicycle-boom`       | `bicycle`                      | 1885-01-01 to 1887-12-31 | [20,059 / 382][A40] | [1885-01-01, New-York Tribune][P40]: Describes bicycle riding among public entertainments.                | [LoC][S38] |
| `roller-skating`     | `roller skating`               | 1884-01-01 to 1886-12-31 |  [8,894 / 341][A41] | [1884-01-01, Morning Journal and Courier][P41]: Advertises a roller-skating attraction in New Haven.      | [LoC][S39] |
| `rock-springs`       | `"Rock Springs" AND Chinese`   | 1885-09-02 to 1885-12-31 |  [1,181 / 203][A42] | [1885-09-03, Sacramento Daily Record-Union][P42]: Reports the attack on Chinese miners in Wyoming.        | [LoC][S40] |
| `dawes-allotment`    | `Dawes AND allotment`          | 1887-01-01 to 1888-12-31 |     [305 / 95][A43] | [1887-01-05, Lancaster Daily Intelligencer][P43]: Explains Dawes's proposed allotment of Indigenous land. | [LoC][S41] |
| `ida-wells`          | `"Ida B Wells" OR "Ida Wells"` | 1892-01-01 to 1894-12-31 |  [1,098 / 169][A44] | [1894-07-07, The Freeman][P44]: Discusses Wells's advocacy and attacks on her work.                       | [LoC][S42] |
| `sissieretta-jones`  | `Black Patti`                  | 1892-01-01 to 1894-12-31 |     [629 / 80][A45] | [1892-04-03, The Sun][P45]: Identifies Jones and discusses her singing engagements.                       | [LoC][S43] |
| `ragtime`            | `ragtime`                      | 1897-01-01 to 1899-12-31 |    [706 / 136][A46] | [1899-04-24, New-York Tribune][P46]: Debates the music's rhythm and cultural origins.                     | [LoC][S44] |
| `passenger-pigeons`  | `passenger pigeon`             | 1880-01-01 to 1882-12-31 |      [52 / 45][A47] | [1880-06-04, The Daily Gazette][P47]: Describes passenger-pigeon roosts and habits.                       | [LoC][S45] |
| `statue-liberty`     | `Bartholdi statue`             | 1885-01-01 to 1886-12-31 |  [2,161 / 287][A48] | [1885-01-01, Sacramento Daily Record-Union][P48]: Recaps France's presentation of the Bartholdi statue.   | [LoC][S46] |
| `nellie-bly`         | `"Nellie Bly" AND world`       | 1889-11-01 to 1890-02-28 |  [1,261 / 258][A49] | [1889-11-23, The Evening World][P49]: Reports Bly visiting Jules Verne during her journey.                | [LoC][S47] |
| `homestead-strike`   | `Homestead strike`             | 1892-01-01 to 1893-12-31 |  [2,074 / 303][A50] | [1892-06-26, Pittsburg Dispatch][P50]: Reports preparations for the steel-industry confrontation.         | [LoC][S48] |
| `x-rays`             | `X rays`                       | 1896-01-01 to 1897-12-31 |  [8,727 / 456][A51] | [1896-01-26, The Sun][P51]: Describes imaging fractures and investigating the new rays.                   | [LoC][S49] |
| `nikola-tesla`       | `Nikola Tesla`                 | 1893-01-01 to 1895-12-31 |    [338 / 143][A52] | [1893-02-15, The Morning Call][P52]: Announces Tesla's planned electrical lecture.                        | [LoC][S50] |
| `esperanto`          | `Esperanto AND language`       | 1897-01-01 to 1900-12-31 |      [62 / 41][A53] | [1899-07-18, Deseret Evening News][P53]: Describes Esperanto as a proposed universal language.            | [LoC][S51] |
| `carnegie-libraries` | `Carnegie library`             | 1889-01-01 to 1891-12-31 |     [858 / 52][A54] | [1889-01-09, Pittsburg Dispatch][P54]: Discusses a prospective Carnegie library site.                     | [LoC][S52] |

### 1900-1929

| ID                      | Query                         | Window                   |      Pages / places | Inspected primary evidence                                                                                            | Context    |
| ----------------------- | ----------------------------- | ------------------------ | ------------------: | --------------------------------------------------------------------------------------------------------------------- | ---------- |
| `electric-cars`         | `electric automobile`         | 1900-01-01 to 1902-12-31 |    [908 / 170][A55] | [1900-01-03, The Abbeville Press and Banner][P55]: Discusses electric vehicles and operating costs.                   | [LoC][S53] |
| `wireless-marconi`      | `Marconi`                     | 1901-01-01 to 1903-12-31 | [17,287 / 595][A56] | [1901-01-01, The Topeka State Journal][P56]: Discusses Marconi's wireless transmission experiments.                   | [LoC][S54] |
| `radium-discovery`      | `radium`                      | 1903-01-01 to 1905-12-31 | [16,400 / 622][A57] | [1903-01-01, The Bee][P57]: Reports scientific investigation of radium's emissions.                                   | [LoC][S55] |
| `halleys-comet`         | `Halley* AND comet`           | 1910-01-01 to 1910-12-31 | [12,369 / 614][A58] | [1910-01-01, The Colorado Statesman][P58]: Discusses Earth's predicted passage through the comet's tail.              | [LoC][S56] |
| `mount-wilson`          | `Mount Wilson`                | 1904-01-01 to 1906-12-31 |     [310 / 93][A59] | [1906-12-29, Passaic City Record][P59]: Describes moving telescope equipment up the mountain.                         | [LoC][S57] |
| `tuberculosis-campaign` | `tuberculosis AND prevention` | 1905-01-01 to 1907-12-31 |  [3,777 / 462][A60] | [1905-01-01, The Sun][P60]: Lists public lectures on tuberculosis prevention.                                         | [LoC][S58] |
| `typhoid-mary`          | `Typhoid Mary`                | 1907-01-01 to 1909-12-31 |      [84 / 49][A61] | [1909-06-30, New-York Tribune][P61]: Reports Mallon's legal challenge to enforced isolation.                          | [LoC][S59] |
| `major-taylor`          | `"Major Taylor" AND bicycle`  | 1900-01-01 to 1902-12-31 |  [1,259 / 113][A62] | [1900-10-02, The Sun][P62]: Identifies Taylor as the professional cycling champion.                                   | [LoC][S60] |
| `jim-thorpe`            | `Jim Thorpe`                  | 1912-01-01 to 1913-12-31 |  [3,097 / 209][A63] | [1913-12-31, Newark Evening Star and Newark Advertiser][P63]: Discusses Thorpe's amateur status and move to baseball. | [LoC][S61] |
| `bessie-coleman`        | `"Bessie Coleman" AND aviat*` | 1922-01-01 to 1926-12-31 |      [60 / 38][A64] | [1922-05-25, The Savannah Tribune][P64]: Reports Coleman's aviation activities in Europe.                             | [LoC][S62] |
| `tulsa-massacre`        | `Tulsa riot`                  | 1921-01-01 to 1921-12-31 |     [154 / 48][A65] | [1921-06-01, The Topeka State Journal][P65]: Reports the Tulsa violence in contemporary terminology.                  | [LoC][S63] |
| `sleeping-car-porters`  | `sleeping car porters`        | 1925-01-01 to 1927-12-31 |     [571 / 31][A66] | [1927-12-31, The Daily Worker][P66]: Reports the Brotherhood's organizing and strike threat.                          | [LoC][S64] |
| `triangle-fire`         | `Triangle fire`               | 1911-01-01 to 1911-12-31 |      [62 / 27][A67] | [1911-12-29, The Madison Daily Leader][P67]: Discusses the factory owners' trial and victims' relatives.              | [LoC][S65] |
| `panama-canal`          | `Panama canal`                | 1914-01-01 to 1914-12-31 | [36,746 / 619][A68] | [1914-01-01, Norwich Bulletin][P68]: Discusses the approaching completion of canal construction.                      | [LoC][S66] |

### 1929-1945

| ID                     | Query                           | Window                   |     Pages / places | Inspected primary evidence                                                                                   | Context                 |
| ---------------------- | ------------------------------- | ------------------------ | -----------------: | ------------------------------------------------------------------------------------------------------------ | ----------------------- |
| `dust-bowl`            | `dust storm`                    | 1934-01-01 to 1936-12-31 |  [1,503 / 85][A69] | [1934-01-13, The Indianapolis Times][P69]: Reports a dust storm in drought-stricken Kansas.                  | [LoC][S67]              |
| `earhart-atlantic`     | `Amelia Earhart`                | 1932-05-20 to 1932-07-31 |    [648 / 61][A70] | [1932-05-20, The Waterbury Democrat][P70]: Reports preparations for Earhart's solo Atlantic flight.          | [Smithsonian][S68]      |
| `tuskegee-airmen`      | `Tuskegee AND "fighter pilots"` | 1943-01-01 to 1945-12-31 |     [55 / 14][A71] | [1943-07-07, Atlanta Daily World][P71]: Reports Black military pilots and the Tuskegee training program.     | [LoC][S69]              |
| `lindbergh-kidnapping` | `Lindbergh baby`                | 1932-01-01 to 1932-12-31 |  [2,241 / 74][A72] | [1932-03-05, The Washington Times][P72]: Reports the kidnapping investigation and search.                    | [LoC][S70]              |
| `scottsboro`           | `Scottsboro`                    | 1931-01-01 to 1933-12-31 |  [4,456 / 65][A73] | [1933-12-30, The Tribune Independent of Michigan][P73]: Reports Patterson and Norris facing death sentences. | [Alabama Archives][S71] |
| `bonneville-dam`       | `Bonneville dam`                | 1937-01-01 to 1939-12-31 |    [471 / 53][A74] | [1937-01-01, The Washington Times][P74]: Discusses legislation to market federal hydroelectric power.        | [NPS][S72]              |
| `golden-gate-bridge`   | `Golden Gate bridge`            | 1937-01-01 to 1937-12-31 |    [300 / 52][A75] | [1937-05-28, The Washington Times][P75]: Announces a broadcast of the bridge's formal opening.               | [Bridge District][S73]  |
| `boulder-dam`          | `Boulder dam`                   | 1935-01-01 to 1936-12-31 |  [2,366 / 99][A76] | [1936-12-31, The Ely Miner][P76]: Recaps the completed dam's transfer to reclamation officials.              | [NPS][S74]              |
| `jesse-owens`          | `Jesse Owens`                   | 1936-08-01 to 1936-09-30 |    [784 / 68][A77] | [1936-08-01, The Waterbury Democrat][P77]: Discusses Owens among Olympic sprint favorites.                   | [Smithsonian][S75]      |
| `marian-anderson`      | `Marian Anderson`               | 1939-03-01 to 1939-05-31 |    [520 / 35][A78] | [1939-03-01, Evening Star][P78]: Reports petitions against the concert-hall exclusion.                       | [NPS][S76]              |
| `penicillin`           | `penicillin`                    | 1943-01-01 to 1945-12-31 | [1,968 / 112][A79] | [1943-01-04, Detroit Evening Times][P79]: Discusses efforts to speed antibiotic production.                  | [Smithsonian][S77]      |
| `bracero-workers`      | `braceros`                      | 1942-01-01 to 1945-12-31 |     [135 / 6][A80] | [1942-11-24, El Sol][P80]: Reports Mexican workers departing for California farms.                           | [Smithsonian][S78]      |
| `victory-gardens`      | `victory garden`                | 1942-01-01 to 1944-12-31 | [9,323 / 135][A81] | [1942-01-08, Evening Star][P81]: Reports a Maryland wartime home-gardening program.                          | [Smithsonian][S79]      |

### 1945-1963

| ID                    | Query                            | Window                   |  Pages / places | Inspected primary evidence                                                                             | Context                      |
| --------------------- | -------------------------------- | ------------------------ | --------------: | ------------------------------------------------------------------------------------------------------ | ---------------------------- |
| `jackie-robinson`     | `Jackie Robinson`                | 1947-01-01 to 1947-12-31 | [943 / 37][A82] | [1947-01-01, The Northwest Enterprise][P82]: Discusses Robinson's prospects with Brooklyn.             | [Baseball Hall of Fame][S80] |
| `ralph-bunche`        | `Ralph Bunche`                   | 1950-01-01 to 1951-12-31 | [639 / 48][A83] | [1951-04-19, Atlanta Daily World][P83]: Discusses Bunche's Nobel Prize and diplomatic stature.         | [LoC][S81]                   |
| `maria-tallchief`     | `Maria Tallchief`                | 1947-01-01 to 1951-12-31 |   [12 / 3][A84] | [1947-02-05, The Washington Daily News][P84]: Reviews Tallchief performing choreography by Balanchine. | [LoC][S82]                   |
| `eartha-kitt`         | `Eartha Kitt`                    | 1953-01-01 to 1955-12-31 | [389 / 35][A85] | [1953-01-09, Arkansas State Press][P85]: Discusses Kitt's recordings and emergence as a star.          | [LoC][S83]                   |
| `roberto-clemente`    | `Roberto Clemente`               | 1955-01-01 to 1957-12-31 | [118 / 10][A86] | [1955-01-05, The Key West Citizen][P86]: Discusses Pittsburgh's newly drafted outfielder.              | [LoC][S84]                   |
| `anna-may-wong`       | `Anna May Wong`                  | 1961-01-01 to 1961-12-31 |    [9 / 5][A87] | [1961-02-04, Evening Star][P87]: Reports Wong's death and summarizes her screen career.                | [LoC][S85]                   |
| `everest-ascent`      | `Everest`                        | 1953-05-29 to 1953-08-31 | [151 / 30][A88] | [1953-06-02, Evening Star][P88]: Reports the successful summit and earlier failed expeditions.         | [RGS][S86]                   |
| `antarctic-treaty`    | `Antarctic treaty`               | 1959-01-01 to 1961-12-31 |   [35 / 8][A89] | [1959-10-15, The Key West Citizen][P89]: Discusses negotiations for peaceful scientific cooperation.   | [Treaty Secretariat][S87]    |
| `geophysical-year`    | `International Geophysical Year` | 1957-01-01 to 1958-12-31 | [565 / 38][A90] | [1958-12-31, Evening Star][P90]: Uses the research program in a year-end newspaper cartoon.            | [National Archives][S88]     |
| `interstate-highways` | `interstate highway`             | 1956-01-01 to 1958-12-31 | [525 / 43][A91] | [1956-01-05, Evening Star][P91]: Discusses proposed federal highway legislation.                       | [National Archives][S89]     |
| `nautilus-submarine`  | `Nautilus AND submarine`         | 1954-01-01 to 1956-12-31 | [371 / 46][A92] | [1954-01-05, The Daily Record][P92]: Discusses the forthcoming nuclear submarine launch.               | [NPS][S90]                   |
| `transistor-radio`    | `transistor radio`               | 1954-01-01 to 1956-12-31 |   [74 / 6][A93] | [1955-01-28, The Catholic Times][P93]: Describes a small radio powered by batteries.                   | [Smithsonian][S91]           |
| `hula-hoop`           | `hula hoop`                      | 1958-01-01 to 1959-12-31 | [326 / 50][A94] | [1958-08-08, Evening Star][P94]: Advertises the newly popular plastic toy.                             | [Smithsonian][S92]           |
| `telstar-satellite`   | `Telstar`                        | 1962-01-01 to 1963-12-31 | [651 / 49][A95] | [1962-03-30, Evening Star][P95]: Describes plans for satellite television exchanges.                   | [Smithsonian][S93]           |

### 1751-1828

| ID                   | Query                          | Window                   |   Pages / places | Inspected primary evidence                                                                                            | Context    |
| -------------------- | ------------------------------ | ------------------------ | ---------------: | --------------------------------------------------------------------------------------------------------------------- | ---------- |
| `albany-union`       | `Albany AND union`             | 1754-01-01 to 1754-12-31 |     [2 / 1][A96] | [1754-05-09, The Pennsylvania Gazette][P96]: Discusses colonial union and the Albany meeting.                         | [LoC][S94] |
| `stamp-act`          | `stamp act`                    | 1765-01-01 to 1766-12-31 |    [79 / 2][A97] | [1765-04-19, The New-Hampshire Gazette and Historical Chronicle][P97]: Reports debate over Parliament's stamp duties. | [MHS][S95] |
| `boston-tea-party`   | `Boston AND tea AND destroyed` | 1773-12-17 to 1774-03-31 |    [22 / 5][A98] | [1773-12-24, The New-Hampshire Gazette and Historical Chronicle][P98]: Reports destruction of the tea at Boston.      | [NPS][S96] |
| `cowpox-vaccination` | `"cow pox" OR "cow pock"`      | 1800-01-01 to 1803-12-31 |  [141 / 10][A99] | [1800-05-22, The Kentucky Gazette][P99]: Explains the new vaccination method against smallpox.                        | [LoC][S97] |
| `erie-canal`         | `Erie canal`                   | 1825-01-01 to 1826-12-31 | [304 / 22][A100] | [1825-01-11, Phenix Gazette][P100]: Reports the canal and its transportation prospects.                               | [LoC][S98] |

## Maintenance

Recheck the last 100 entries of `examples.json` against a future index before
relying on these counts. Read `/v1/meta`, then request `/v1/aggregate?` followed by
each entry's `aggregate` string, keeping no more than two requests in flight.
Poll the same URL after a 202, respect `Retry-After`, and stop or narrow the
window rather than repeatedly restarting an expensive query. Check
`total.hits`, `total.places`, `index_version`, and the first/last snippets; use
place-scoped `/v1/hits` with `sort=relevant` when context is ambiguous.

The focused example tests check the total of 200, unique search expressions,
IDs and copy, URL round-tripping, exact aggregate/view parity, parser limits,
date bounds, and randomized era rotation. Original-entry preservation was also
checked by comparing the first 100 objects with the parent commit.

The API embeds this same file for pre-warming. Doubling the catalogue does not
double its time budgets; some queries may remain cold until a later start or
visitor request. The historical 100-example timings in the serving documents
must not be treated as a measured result for these 200 presets.

[directory]: https://guides.loc.gov/chronicling-america-topics/alphabetical-order
[S1]: https://siarchives.si.edu/collections/siris_arc_367731
[S2]: https://npg.si.edu/learn/classroom-resource/men-progress
[S3]: https://blogs.loc.gov/loc/2020/01/inventing-the-modern-world/
[S4]: https://guides.loc.gov/chronicling-america-asylums
[S5]: https://guides.loc.gov/chronicling-america-dorr-rebellion
[S6]: https://guides.loc.gov/chronicling-america-amelia-bloomer
[S7]: https://guides.loc.gov/chronicling-america-darwin-theory-of-evolution
[S8]: https://guides.loc.gov/chronicling-america-walt-whitman
[S9]: https://guides.loc.gov/chronicling-america-early-utopian-communities
[S10]: https://guides.loc.gov/chronicling-america-siamese-twins
[S11]: https://guides.loc.gov/chronicling-america-circus
[S12]: https://guides.loc.gov/chronicling-america-civil-war-ballooning
[S13]: https://guides.loc.gov/chronicling-america-blockade-runners
[S14]: https://www.nps.gov/clba/learn/kidsyouth/chron2.htm
[S15]: https://guides.loc.gov/chronicling-america-sinking-sultana
[S16]: https://guides.loc.gov/chronicling-america-belle-boyd
[S17]: https://www.nps.gov/articles/000/olmsted-and-the-united-states-sanitary-commission.htm
[S18]: https://guides.loc.gov/chronicling-america-edwin-booth
[S19]: https://www.archives.gov/milestone-documents/homestead-act
[S20]: https://www.archives.gov/milestone-documents/morrill-act
[S21]: https://www.nps.gov/gett/learn/historyculture/civil-war-timeline.htm
[S22]: https://www.nps.gov/articles/000/education-during-the-port-royal-experiment.htm
[S23]: https://www.nps.gov/sand/learn/historyculture/index.htm
[S24]: https://guides.loc.gov/chronicling-america-yosemite
[S25]: https://guides.loc.gov/chronicling-america-alaska-purchase
[S26]: https://guides.loc.gov/chronicling-america-fenian-invasions-canada
[S27]: https://guides.loc.gov/chronicling-america-brooklyn-theater-fire
[S28]: https://guides.loc.gov/chronicling-america-yellowstone
[S29]: https://prologue.blogs.archives.gov/2018/08/22/the-navajo-treaty-of-1868-a-personal-story/
[S30]: https://guides.loc.gov/chronicling-america-telephone-invention
[S31]: https://baseballhall.org/discover/pro-baseball-began-in-cincinnati-in-1869
[S32]: https://americanhistory.si.edu/collections/object/nmah_850542
[S33]: https://guides.loc.gov/chronicling-america-brooklyn-bridge
[S34]: https://guides.loc.gov/chronicling-america-molly-maguires
[S35]: https://www.kshs.org/kansapedia/grasshopper-plague-of-1874/12011
[S36]: https://oceanexplorer.noaa.gov/history/timeline-the-breakthrough-years-1866-1922/
[S37]: https://www.sil.si.edu/Exhibitions/Make-the-Dirt-Fly/whybuild.html
[S38]: https://guides.loc.gov/chronicling-america-bicycle-craze
[S39]: https://guides.loc.gov/chronicling-america-roller-skating-craze
[S40]: https://guides.loc.gov/chronicling-america-rock-springs-massacre
[S41]: https://guides.loc.gov/chronicling-america-dawes-act-commission
[S42]: https://guides.loc.gov/chronicling-america-ida-wells
[S43]: https://guides.loc.gov/chronicling-america-sissieretta-jones
[S44]: https://guides.loc.gov/chronicling-america-ragtime
[S45]: https://guides.loc.gov/chronicling-america-passenger-pigeons
[S46]: https://guides.loc.gov/chronicling-america-statue-of-liberty
[S47]: https://guides.loc.gov/chronicling-america-nellie-bly
[S48]: https://guides.loc.gov/chronicling-america-homestead-strike
[S49]: https://guides.loc.gov/chronicling-america-x-rays
[S50]: https://guides.loc.gov/chronicling-america-nikola-tesla
[S51]: https://guides.loc.gov/chronicling-america-esperanto
[S52]: https://guides.loc.gov/chronicling-america-carnegie-libraries
[S53]: https://guides.loc.gov/chronicling-america-early-electric-cars
[S54]: https://guides.loc.gov/chronicling-america-guglielmo-marconi
[S55]: https://guides.loc.gov/chronicling-america-radium
[S56]: https://guides.loc.gov/chronicling-america-halleys-comet
[S57]: https://guides.loc.gov/chronicling-america-mount-wilson-observatory
[S58]: https://guides.loc.gov/chronicling-america-tuberculosis
[S59]: https://guides.loc.gov/chronicling-america-typhoid-mary
[S60]: https://guides.loc.gov/chronicling-america-major-taylor
[S61]: https://guides.loc.gov/chronicling-america-jim-thorpe
[S62]: https://guides.loc.gov/chronicling-america-bessie-coleman
[S63]: https://guides.loc.gov/chronicling-america-tulsa-race-riots
[S64]: https://guides.loc.gov/chronicling-america-pullman-porters
[S65]: https://guides.loc.gov/chronicling-america-triangle-shirtwaist-factory-fire
[S66]: https://guides.loc.gov/chronicling-america-panama-canal
[S67]: https://guides.loc.gov/chronicling-america-dust-bowl
[S68]: https://airandspace.si.edu/collection-objects/lockheed-vega-5b-amelia-earhart/nasm_A19670093000
[S69]: https://guides.loc.gov/chronicling-america-tuskegee-airmen
[S70]: https://guides.loc.gov/chronicling-america-lindbergh-baby-kidnapping
[S71]: https://digital.archives.alabama.gov/digital/collection/voices/id/22832/
[S72]: https://npgallery.nps.gov/NRHP/GetAsset/NHLS/86000727_text
[S73]: https://www.goldengate.org/bridge/history-research/statistics-data/faqs/
[S74]: https://www.nps.gov/articles/nevada-and-arizona-hoover-dam.htm
[S75]: https://nmaahc.si.edu/explore/stories/african-americans-olympic-games
[S76]: https://www.nps.gov/articles/000/marian-anderson-and-constitution-hall.htm
[S77]: https://americanhistory.si.edu/collections/object/nmah_730390
[S78]: https://americanhistory.si.edu/explore/exhibitions/bittersweet-harvest-bracero-program-1942-1964
[S79]: https://gardens.si.edu/timeline/1942-1945-victory-gardens/
[S80]: https://baseballhall.org/discover-more/stories/baseball-history/remembering-jackie
[S81]: https://guides.loc.gov/chronicling-america-ralph-bunche
[S82]: https://guides.loc.gov/chronicling-america-maria-tallchief
[S83]: https://guides.loc.gov/chronicling-america-eartha-kitt
[S84]: https://guides.loc.gov/chronicling-america-roberto-clemente
[S85]: https://guides.loc.gov/chronicling-america-anna-may-wong
[S86]: https://www.rgs.org/our-collections/buy-and-license-images/platinum-prints/everest-1953
[S87]: https://www.ats.aq/e/antarctictreaty.html
[S88]: https://www.archives.gov/research/guide-fed-records/groups/307.html
[S89]: https://www.archives.gov/milestone-documents/national-interstate-and-defense-highways-act
[S90]: https://npgallery.nps.gov/NRHP/GetAsset/NHLS/79002653_text
[S91]: https://americanhistory.si.edu/collections/object/nmah_713528
[S92]: https://www.si.edu/stories/invention-american-backyard
[S93]: https://airandspace.si.edu/collection-objects/communications-satellite-telstar/nasm_A20070113000
[S94]: https://guides.loc.gov/chronicling-america-join-or-die
[S95]: https://www.masshist.org/revolution/stamp.php
[S96]: https://www.nps.gov/places/old-south-meeting-house.htm
[S97]: https://blogs.loc.gov/inside_adams/2019/02/smallpox-pioneer-edward-jenners-dear-friend-celebrated-physician-caleb-hillier-parry/
[S98]: https://blogs.loc.gov/inside_adams/2011/10/15-years-on-the-erie-canal/
[A1]: https://usnewsmap.com/v1/aggregate?q=daguerreotype&mode=phrase&from=1839-01-01&to=1841-12-31&bucket=month
[P1]: https://www.loc.gov/resource/sn83026172/1839-09-25/ed-1/?sp=2&q=daguerreotype
[A2]: https://usnewsmap.com/v1/aggregate?q=reaping+machine&mode=phrase&from=1851-01-01&to=1853-12-31&bucket=month
[P2]: https://www.loc.gov/resource/sn83016348/1851-02-22/ed-1/?sp=2&q=reaping+machine
[A3]: https://usnewsmap.com/v1/aggregate?q=sewing+machine&mode=phrase&from=1851-01-01&to=1853-12-31&bucket=month
[P3]: https://www.loc.gov/resource/sn83030213/1851-01-01/ed-1/?sp=2&q=sewing+machine
[A4]: https://usnewsmap.com/v1/aggregate?q=Goodyear+AND+rubber&from=1845-01-01&to=1847-12-31&bucket=month
[P4]: https://www.loc.gov/resource/sn83030213/1847-12-31/ed-1/?sp=4&q=goodyear+rubber
[A5]: https://usnewsmap.com/v1/aggregate?q=electric+telegraph&mode=phrase&from=1844-01-01&to=1846-12-31&bucket=month
[P5]: https://www.loc.gov/resource/sn84022465/1844-02-07/ed-1/?sp=3&q=electric+telegraph
[A6]: https://usnewsmap.com/v1/aggregate?q=insane+asylum&mode=phrase&from=1843-01-01&to=1845-12-31&bucket=month
[P6]: https://www.loc.gov/resource/sn83030313/1843-01-02/ed-1/?sp=6&q=insane+asylum
[A7]: https://usnewsmap.com/v1/aggregate?q=%22Dorr%22+AND+rebellion&from=1842-01-01&to=1843-12-31&bucket=month
[P7]: https://www.loc.gov/resource/sn82015015/1842-05-21/ed-1/?sp=2&q=dorr+rebellion
[A8]: https://usnewsmap.com/v1/aggregate?q=Bloomer+costume&mode=phrase&from=1851-01-01&to=1853-12-31&bucket=month
[P8]: https://www.loc.gov/resource/sn84023252/1851-05-31/ed-1/?sp=3&q=bloomer+costume
[A9]: https://usnewsmap.com/v1/aggregate?q=Origin+of+Species&mode=phrase&from=1859-01-01&to=1860-12-31&bucket=month
[P9]: https://www.loc.gov/resource/sn83026172/1860-08-06/ed-1/?sp=2&q=origin+of+species
[A10]: https://usnewsmap.com/v1/aggregate?q=Leaves+of+Grass&mode=phrase&from=1855-01-01&to=1857-12-31&bucket=month
[P10]: https://www.loc.gov/resource/sn83030213/1855-07-09/ed-1/?sp=1&q=leaves+of+grass
[A11]: https://usnewsmap.com/v1/aggregate?q=Brook+Farm&mode=phrase&from=1841-01-01&to=1845-12-31&bucket=month
[P11]: https://www.loc.gov/resource/sn83030213/1842-08-13/ed-1/?sp=1&q=brook+farm
[A12]: https://usnewsmap.com/v1/aggregate?q=Chang+and+Eng&mode=phrase&from=1830-01-01&to=1832-12-31&bucket=month
[P12]: https://www.loc.gov/resource/sn83026172/1831-08-09/ed-1/?sp=2&q=chang+and+eng
[A13]: https://usnewsmap.com/v1/aggregate?q=menagerie&mode=phrase&from=1840-01-01&to=1842-12-31&bucket=month
[P13]: https://www.loc.gov/resource/sn83026172/1842-12-23/ed-1/?sp=3&q=menagerie
[A14]: https://usnewsmap.com/v1/aggregate?q=balloon+corps&mode=phrase&from=1861-01-01&to=1863-12-31&bucket=month
[P14]: https://www.loc.gov/resource/sn84028745/1861-10-18/ed-1/?sp=3&q=balloon+corps
[A15]: https://usnewsmap.com/v1/aggregate?q=blockade+runner&mode=phrase&from=1863-01-01&to=1864-12-31&bucket=month
[P15]: https://www.loc.gov/resource/sn83030213/1863-01-13/ed-1/?sp=1&q=blockade+runner
[A16]: https://usnewsmap.com/v1/aggregate?q=Clara+Barton&mode=phrase&from=1863-01-01&to=1865-12-31&bucket=month
[P16]: https://www.loc.gov/resource/sn83021205/1863-07-21/ed-1/?sp=2&q=clara+barton
[A17]: https://usnewsmap.com/v1/aggregate?q=Sultana+AND+explosion&from=1865-04-27&to=1865-07-31&bucket=month
[P17]: https://www.loc.gov/resource/sn83045462/1865-04-29/ed-1/?sp=1&q=explosion+sultana
[A18]: https://usnewsmap.com/v1/aggregate?q=Belle+Boyd&mode=phrase&from=1862-01-01&to=1864-12-31&bucket=month
[P18]: https://www.loc.gov/resource/sn83045462/1862-05-31/ed-1/?sp=2&q=belle+boyd
[A19]: https://usnewsmap.com/v1/aggregate?q=sanitary+commission&mode=phrase&from=1861-01-01&to=1863-12-31&bucket=month
[P19]: https://www.loc.gov/resource/sn83026172/1861-05-21/ed-1/?sp=3&q=sanitary+commission
[A20]: https://usnewsmap.com/v1/aggregate?q=Edwin+Booth&mode=phrase&from=1863-01-01&to=1864-12-31&bucket=month
[P20]: https://www.loc.gov/resource/sn84023814/1863-01-09/ed-1/?sp=2&q=edwin+booth
[A21]: https://usnewsmap.com/v1/aggregate?q=homestead+act&mode=phrase&from=1862-01-01&to=1863-12-31&bucket=month
[P21]: https://www.loc.gov/resource/sn84031490/1862-05-02/ed-1/?sp=1&q=homestead+act
[A22]: https://usnewsmap.com/v1/aggregate?q=agricultural+colleges&mode=phrase&from=1862-01-01&to=1863-12-31&bucket=month
[P22]: https://www.loc.gov/resource/sn84022046/1863-12-31/ed-1/?sp=3&q=agricultural+colleges
[A23]: https://usnewsmap.com/v1/aggregate?q=%22New+York%22+AND+%22draft+riots%22&from=1863-07-13&to=1863-09-30&bucket=month
[P23]: https://www.loc.gov/resource/sn84023210/1863-09-26/ed-1/?sp=2&q=draft+riots+new+york
[A24]: https://usnewsmap.com/v1/aggregate?q=%22Port+Royal%22+AND+freedmen+AND+school%2A&from=1862-01-01&to=1864-12-31&bucket=month
[P24]: https://www.loc.gov/resource/sn83030213/1863-08-06/ed-1/?sp=2&q=port+royal+freedmen+school
[A25]: https://usnewsmap.com/v1/aggregate?q=%22Sand+Creek%22+AND+massacre&from=1864-11-29&to=1865-12-31&bucket=month
[P25]: https://www.loc.gov/resource/sn85029094/1865-12-30/ed-1/?sp=2&q=sand+creek+massacre
[A26]: https://usnewsmap.com/v1/aggregate?q=Yosemite+valley&mode=phrase&from=1864-01-01&to=1865-12-31&bucket=month
[P26]: https://www.loc.gov/resource/sn82014487/1865-12-30/ed-1/?sp=2&q=yosemite+valley
[A27]: https://usnewsmap.com/v1/aggregate?q=Russian+America&mode=phrase&from=1867-01-01&to=1868-12-31&bucket=month
[P27]: https://www.loc.gov/resource/sn86079037/1868-12-31/ed-1/?sp=2&q=russian+america
[A28]: https://usnewsmap.com/v1/aggregate?q=Fenian+AND+Canada&from=1866-05-01&to=1866-07-31&bucket=month
[P28]: https://www.loc.gov/resource/sn86081096/1866-07-31/ed-1/?sp=2&q=canada+fenian
[A29]: https://usnewsmap.com/v1/aggregate?q=%22Brooklyn%22+AND+%22theatre+fire%22&from=1876-12-05&to=1877-02-28&bucket=month
[P29]: https://www.loc.gov/resource/sn83030313/1876-12-07/ed-1/?sp=3&q=theatre+fire+brooklyn
[A30]: https://usnewsmap.com/v1/aggregate?q=Yellowstone+park&mode=phrase&from=1872-01-01&to=1873-12-31&bucket=month
[P30]: https://www.loc.gov/resource/sn83030313/1872-01-24/ed-1/?sp=3&q=yellowstone+park
[A31]: https://usnewsmap.com/v1/aggregate?q=Navajo+AND+treaty&from=1868-01-01&to=1869-12-31&bucket=month
[P31]: https://www.loc.gov/resource/sn83030313/1868-11-20/ed-1/?sp=8&q=navajo+treaty
[A32]: https://usnewsmap.com/v1/aggregate?q=telephone&mode=phrase&from=1876-01-01&to=1877-12-31&bucket=month
[P32]: https://www.loc.gov/resource/sn85033295/1876-01-26/ed-1/?sp=1&q=telephone
[A33]: https://usnewsmap.com/v1/aggregate?q=%22Red+Stockings%22+AND+Cincinnati&from=1869-01-01&to=1870-12-31&bucket=month
[P33]: https://www.loc.gov/resource/sn83030272/1870-10-17/ed-1/?sp=3&q=red+stockings+cincinnati
[A34]: https://usnewsmap.com/v1/aggregate?q=type+writer&mode=phrase&from=1873-01-01&to=1875-12-31&bucket=month
[P34]: https://www.loc.gov/resource/sn84031492/1873-02-10/ed-1/?sp=1&q=type+writer
[A35]: https://usnewsmap.com/v1/aggregate?q=East+river+bridge&mode=phrase&from=1870-01-01&to=1872-12-31&bucket=month
[P35]: https://www.loc.gov/resource/sn83030313/1872-12-29/ed-1/?sp=10&q=east+river+bridge
[A36]: https://usnewsmap.com/v1/aggregate?q=Molly+Maguires&mode=phrase&from=1875-01-01&to=1877-12-31&bucket=month
[P36]: https://www.loc.gov/resource/sn83030214/1877-12-31/ed-1/?sp=1&q=molly+maguires
[A37]: https://usnewsmap.com/v1/aggregate?q=grasshoppers&mode=phrase&from=1874-01-01&to=1875-12-31&bucket=month
[P37]: https://www.loc.gov/resource/sn84038123/1875-12-31/ed-1/?sp=3&q=grasshoppers
[A38]: https://usnewsmap.com/v1/aggregate?q=Challenger+expedition&mode=phrase&from=1873-01-01&to=1876-12-31&bucket=month
[P38]: https://www.loc.gov/resource/sn83030214/1873-02-13/ed-1/?sp=6&q=challenger+expedition
[A39]: https://usnewsmap.com/v1/aggregate?q=Suez+canal&mode=phrase&from=1869-01-01&to=1870-12-31&bucket=month
[P39]: https://www.loc.gov/resource/sn82015775/1869-01-01/ed-1/?sp=2&q=suez+canal
[A40]: https://usnewsmap.com/v1/aggregate?q=bicycle&mode=phrase&from=1885-01-01&to=1887-12-31&bucket=month
[P40]: https://www.loc.gov/resource/sn83030214/1885-01-01/ed-1/?sp=8&q=bicycle
[A41]: https://usnewsmap.com/v1/aggregate?q=roller+skating&mode=phrase&from=1884-01-01&to=1886-12-31&bucket=month
[P41]: https://www.loc.gov/resource/sn82015483/1884-01-01/ed-1/?sp=3&q=roller+skating
[A42]: https://usnewsmap.com/v1/aggregate?q=%22Rock+Springs%22+AND+Chinese&from=1885-09-02&to=1885-12-31&bucket=month
[P42]: https://www.loc.gov/resource/sn82014381/1885-09-03/ed-1/?sp=1&q=rock+springs+chinese
[A43]: https://usnewsmap.com/v1/aggregate?q=Dawes+AND+allotment&from=1887-01-01&to=1888-12-31&bucket=month
[P43]: https://www.loc.gov/resource/sn83032300/1887-01-05/ed-1/?sp=1&q=allotment+dawes
[A44]: https://usnewsmap.com/v1/aggregate?q=%22Ida+B+Wells%22+OR+%22Ida+Wells%22&from=1892-01-01&to=1894-12-31&bucket=month
[P44]: https://www.loc.gov/resource/sn82016211/1894-07-07/ed-1/?sp=6&q=ida+b+wells+ida+wells
[A45]: https://usnewsmap.com/v1/aggregate?q=Black+Patti&mode=phrase&from=1892-01-01&to=1894-12-31&bucket=month
[P45]: https://www.loc.gov/resource/sn83030272/1892-04-03/ed-1/?sp=19&q=black+patti
[A46]: https://usnewsmap.com/v1/aggregate?q=ragtime&mode=phrase&from=1897-01-01&to=1899-12-31&bucket=month
[P46]: https://www.loc.gov/resource/sn83030214/1899-04-24/ed-1/?sp=6&q=ragtime
[A47]: https://usnewsmap.com/v1/aggregate?q=passenger+pigeon&mode=phrase&from=1880-01-01&to=1882-12-31&bucket=month
[P47]: https://www.loc.gov/resource/sn82014805/1880-06-04/ed-1/?sp=3&q=passenger+pigeon
[A48]: https://usnewsmap.com/v1/aggregate?q=Bartholdi+statue&mode=phrase&from=1885-01-01&to=1886-12-31&bucket=month
[P48]: https://www.loc.gov/resource/sn82014381/1885-01-01/ed-1/?sp=6&q=bartholdi+statue
[A49]: https://usnewsmap.com/v1/aggregate?q=%22Nellie+Bly%22+AND+world&from=1889-11-01&to=1890-02-28&bucket=month
[P49]: https://www.loc.gov/resource/sn83030193/1889-11-23/ed-3/?sp=1&q=nellie+bly+world
[A50]: https://usnewsmap.com/v1/aggregate?q=Homestead+strike&mode=phrase&from=1892-01-01&to=1893-12-31&bucket=month
[P50]: https://www.loc.gov/resource/sn84024546/1892-06-26/ed-1/?sp=1&q=homestead+strike
[A51]: https://usnewsmap.com/v1/aggregate?q=X+rays&mode=phrase&from=1896-01-01&to=1897-12-31&bucket=month
[P51]: https://www.loc.gov/resource/sn83030272/1896-01-26/ed-1/?sp=1&q=x+rays
[A52]: https://usnewsmap.com/v1/aggregate?q=Nikola+Tesla&mode=phrase&from=1893-01-01&to=1895-12-31&bucket=month
[P52]: https://www.loc.gov/resource/sn94052989/1893-02-15/ed-1/?sp=4&q=nikola+tesla
[A53]: https://usnewsmap.com/v1/aggregate?q=Esperanto+AND+language&from=1897-01-01&to=1900-12-31&bucket=month
[P53]: https://www.loc.gov/resource/sn83045555/1899-07-18/ed-1/?sp=5&q=esperanto+language
[A54]: https://usnewsmap.com/v1/aggregate?q=Carnegie+library&mode=phrase&from=1889-01-01&to=1891-12-31&bucket=month
[P54]: https://www.loc.gov/resource/sn84024546/1889-01-09/ed-1/?sp=1&q=carnegie+library
[A55]: https://usnewsmap.com/v1/aggregate?q=electric+automobile&mode=phrase&from=1900-01-01&to=1902-12-31&bucket=month
[P55]: https://www.loc.gov/resource/sn84026853/1900-01-03/ed-1/?sp=6&q=electric+automobile
[A56]: https://usnewsmap.com/v1/aggregate?q=Marconi&mode=phrase&from=1901-01-01&to=1903-12-31&bucket=month
[P56]: https://www.loc.gov/resource/sn82016014/1901-01-01/ed-1/?sp=5&q=marconi
[A57]: https://usnewsmap.com/v1/aggregate?q=radium&mode=phrase&from=1903-01-01&to=1905-12-31&bucket=month
[P57]: https://www.loc.gov/resource/sn87060004/1903-01-01/ed-1/?sp=7&q=radium
[A58]: https://usnewsmap.com/v1/aggregate?q=Halley%2A+AND+comet&from=1910-01-01&to=1910-12-31&bucket=month
[P58]: https://www.loc.gov/resource/sn83025514/1910-01-01/ed-1/?sp=7&q=comet+halley
[A59]: https://usnewsmap.com/v1/aggregate?q=Mount+Wilson&mode=phrase&from=1904-01-01&to=1906-12-31&bucket=month
[P59]: https://www.loc.gov/resource/sn85035722/1906-12-29/ed-1/?sp=4&q=mount+wilson
[A60]: https://usnewsmap.com/v1/aggregate?q=tuberculosis+AND+prevention&from=1905-01-01&to=1907-12-31&bucket=month
[P60]: https://www.loc.gov/resource/sn83030272/1905-01-01/ed-1/?sp=11&q=prevention+tuberculosis
[A61]: https://usnewsmap.com/v1/aggregate?q=Typhoid+Mary&mode=phrase&from=1907-01-01&to=1909-12-31&bucket=month
[P61]: https://www.loc.gov/resource/sn83030214/1909-06-30/ed-1/?sp=2&q=typhoid+mary
[A62]: https://usnewsmap.com/v1/aggregate?q=%22Major+Taylor%22+AND+bicycle&from=1900-01-01&to=1902-12-31&bucket=month
[P62]: https://www.loc.gov/resource/sn83030272/1900-10-02/ed-1/?sp=9&q=major+taylor+bicycle
[A63]: https://usnewsmap.com/v1/aggregate?q=Jim+Thorpe&mode=phrase&from=1912-01-01&to=1913-12-31&bucket=month
[P63]: https://www.loc.gov/resource/sn91064011/1913-12-31/ed-1/?sp=13&q=jim+thorpe
[A64]: https://usnewsmap.com/v1/aggregate?q=%22Bessie+Coleman%22+AND+aviat%2A&from=1922-01-01&to=1926-12-31&bucket=month
[P64]: https://www.loc.gov/resource/sn84020323/1922-05-25/ed-1/?sp=3&q=bessie+coleman+aviat
[A65]: https://usnewsmap.com/v1/aggregate?q=Tulsa+riot&mode=phrase&from=1921-01-01&to=1921-12-31&bucket=month
[P65]: https://www.loc.gov/resource/sn82016014/1921-06-01/ed-1/?sp=2&q=tulsa+riot
[A66]: https://usnewsmap.com/v1/aggregate?q=sleeping+car+porters&mode=phrase&from=1925-01-01&to=1927-12-31&bucket=month
[P66]: https://www.loc.gov/resource/sn84020097/1927-12-31/ed-1/?sp=8&q=sleeping+car+porters
[A67]: https://usnewsmap.com/v1/aggregate?q=Triangle+fire&mode=phrase&from=1911-01-01&to=1911-12-31&bucket=month
[P67]: https://www.loc.gov/resource/sn99062034/1911-12-29/ed-1/?sp=2&q=triangle+fire
[A68]: https://usnewsmap.com/v1/aggregate?q=Panama+canal&mode=phrase&from=1914-01-01&to=1914-12-31&bucket=month
[P68]: https://www.loc.gov/resource/sn82014086/1914-01-01/ed-1/?sp=1&q=panama+canal
[A69]: https://usnewsmap.com/v1/aggregate?q=dust+storm&mode=phrase&from=1934-01-01&to=1936-12-31&bucket=month
[P69]: https://www.loc.gov/resource/sn82015313/1934-01-13/ed-1/?sp=14&q=dust+storm
[A70]: https://usnewsmap.com/v1/aggregate?q=Amelia+Earhart&mode=phrase&from=1932-05-20&to=1932-07-31&bucket=month
[P70]: https://www.loc.gov/resource/sn82014085/1932-05-20/ed-1/?sp=1&q=amelia+earhart
[A71]: https://usnewsmap.com/v1/aggregate?q=Tuskegee+AND+%22fighter+pilots%22&from=1943-01-01&to=1945-12-31&bucket=month
[P71]: https://www.loc.gov/resource/sn82015425/1943-07-07/ed-1/?sp=1&q=fighter+pilots+tuskegee
[A72]: https://usnewsmap.com/v1/aggregate?q=Lindbergh+baby&mode=phrase&from=1932-01-01&to=1932-12-31&bucket=month
[P72]: https://www.loc.gov/resource/sn84026749/1932-03-05/ed-1/?sp=3&q=lindbergh+baby
[A73]: https://usnewsmap.com/v1/aggregate?q=Scottsboro&mode=phrase&from=1931-01-01&to=1933-12-31&bucket=month
[P73]: https://www.loc.gov/resource/sn96076588/1933-12-30/ed-1/?sp=2&q=scottsboro
[A74]: https://usnewsmap.com/v1/aggregate?q=Bonneville+dam&mode=phrase&from=1937-01-01&to=1939-12-31&bucket=month
[P74]: https://www.loc.gov/resource/sn84026749/1937-01-01/ed-1/?sp=17&q=bonneville+dam
[A75]: https://usnewsmap.com/v1/aggregate?q=Golden+Gate+bridge&mode=phrase&from=1937-01-01&to=1937-12-31&bucket=month
[P75]: https://www.loc.gov/resource/sn84026749/1937-05-28/ed-1/?sp=31&q=golden+gate+bridge
[A76]: https://usnewsmap.com/v1/aggregate?q=Boulder+dam&mode=phrase&from=1935-01-01&to=1936-12-31&bucket=month
[P76]: https://www.loc.gov/resource/sn90059182/1936-12-31/ed-1/?sp=2&q=boulder+dam
[A77]: https://usnewsmap.com/v1/aggregate?q=Jesse+Owens&mode=phrase&from=1936-08-01&to=1936-09-30&bucket=month
[P77]: https://www.loc.gov/resource/sn82014085/1936-08-01/ed-1/?sp=12&q=jesse+owens
[A78]: https://usnewsmap.com/v1/aggregate?q=Marian+Anderson&mode=phrase&from=1939-03-01&to=1939-05-31&bucket=month
[P78]: https://www.loc.gov/resource/sn83045462/1939-03-01/ed-1/?sp=3&q=marian+anderson
[A79]: https://usnewsmap.com/v1/aggregate?q=penicillin&mode=phrase&from=1943-01-01&to=1945-12-31&bucket=month
[P79]: https://www.loc.gov/resource/sn88063294/1943-01-04/ed-1/?sp=8&q=penicillin
[A80]: https://usnewsmap.com/v1/aggregate?q=braceros&mode=phrase&from=1942-01-01&to=1945-12-31&bucket=month
[P80]: https://www.loc.gov/resource/sn86090862/1942-11-24/ed-1/?sp=2&q=braceros
[A81]: https://usnewsmap.com/v1/aggregate?q=victory+garden&mode=phrase&from=1942-01-01&to=1944-12-31&bucket=month
[P81]: https://www.loc.gov/resource/sn83045462/1942-01-08/ed-1/?sp=51&q=victory+garden
[A82]: https://usnewsmap.com/v1/aggregate?q=Jackie+Robinson&mode=phrase&from=1947-01-01&to=1947-12-31&bucket=month
[P82]: https://www.loc.gov/resource/sn87093377/1947-01-01/ed-1/?sp=1&q=jackie+robinson
[A83]: https://usnewsmap.com/v1/aggregate?q=Ralph+Bunche&mode=phrase&from=1950-01-01&to=1951-12-31&bucket=month
[P83]: https://www.loc.gov/resource/sn82015425/1951-04-19/ed-1/?sp=6&q=ralph+bunche
[A84]: https://usnewsmap.com/v1/aggregate?q=Maria+Tallchief&mode=phrase&from=1947-01-01&to=1951-12-31&bucket=month
[P84]: https://www.loc.gov/resource/sn82016181/1947-02-05/ed-1/?sp=43&q=maria+tallchief
[A85]: https://usnewsmap.com/v1/aggregate?q=Eartha+Kitt&mode=phrase&from=1953-01-01&to=1955-12-31&bucket=month
[P85]: https://www.loc.gov/resource/sn84025840/1953-01-09/ed-1/?sp=7&q=eartha+kitt
[A86]: https://usnewsmap.com/v1/aggregate?q=Roberto+Clemente&mode=phrase&from=1955-01-01&to=1957-12-31&bucket=month
[P86]: https://www.loc.gov/resource/sn83016244/1955-01-05/ed-1/?sp=6&q=roberto+clemente
[A87]: https://usnewsmap.com/v1/aggregate?q=Anna+May+Wong&mode=phrase&from=1961-01-01&to=1961-12-31&bucket=month
[P87]: https://www.loc.gov/resource/sn83045462/1961-02-04/ed-1/?sp=10&q=anna+may+wong
[A88]: https://usnewsmap.com/v1/aggregate?q=Everest&mode=phrase&from=1953-05-29&to=1953-08-31&bucket=month
[P88]: https://www.loc.gov/resource/sn83045462/1953-06-02/ed-1/?sp=3&q=everest
[A89]: https://usnewsmap.com/v1/aggregate?q=Antarctic+treaty&mode=phrase&from=1959-01-01&to=1961-12-31&bucket=month
[P89]: https://www.loc.gov/resource/sn83016244/1959-10-15/ed-1/?sp=1&q=antarctic+treaty
[A90]: https://usnewsmap.com/v1/aggregate?q=International+Geophysical+Year&mode=phrase&from=1957-01-01&to=1958-12-31&bucket=month
[P90]: https://www.loc.gov/resource/sn83045462/1958-12-31/ed-1/?sp=5&q=international+geophysical+year
[A91]: https://usnewsmap.com/v1/aggregate?q=interstate+highway&mode=phrase&from=1956-01-01&to=1958-12-31&bucket=month
[P91]: https://www.loc.gov/resource/sn83045462/1956-01-05/ed-1/?sp=6&q=interstate+highway
[A92]: https://usnewsmap.com/v1/aggregate?q=Nautilus+AND+submarine&from=1954-01-01&to=1956-12-31&bucket=month
[P92]: https://www.loc.gov/resource/sn88063132/1954-01-05/ed-1/?sp=4&q=nautilus+submarine
[A93]: https://usnewsmap.com/v1/aggregate?q=transistor+radio&mode=phrase&from=1954-01-01&to=1956-12-31&bucket=month
[P93]: https://www.loc.gov/resource/sn83007243/1955-01-28/ed-1/?sp=7&q=transistor+radio
[A94]: https://usnewsmap.com/v1/aggregate?q=hula+hoop&mode=phrase&from=1958-01-01&to=1959-12-31&bucket=month
[P94]: https://www.loc.gov/resource/sn83045462/1958-08-08/ed-1/?sp=14&q=hula+hoop
[A95]: https://usnewsmap.com/v1/aggregate?q=Telstar&mode=phrase&from=1962-01-01&to=1963-12-31&bucket=month
[P95]: https://www.loc.gov/resource/sn83045462/1962-03-30/ed-1/?sp=5&q=telstar
[A96]: https://usnewsmap.com/v1/aggregate?q=Albany+AND+union&from=1754-01-01&to=1754-12-31&bucket=month
[P96]: https://www.loc.gov/resource/sn84026371/1754-05-09/ed-1/?sp=1&q=albany+union
[A97]: https://usnewsmap.com/v1/aggregate?q=stamp+act&mode=phrase&from=1765-01-01&to=1766-12-31&bucket=month
[P97]: https://www.loc.gov/resource/sn83025582/1765-04-19/ed-1/?sp=2&q=stamp+act
[A98]: https://usnewsmap.com/v1/aggregate?q=Boston+AND+tea+AND+destroyed&from=1773-12-17&to=1774-03-31&bucket=month
[P98]: https://www.loc.gov/resource/sn83025582/1773-12-24/ed-1/?sp=3&q=boston+destroyed+tea
[A99]: https://usnewsmap.com/v1/aggregate?q=%22cow+pox%22+OR+%22cow+pock%22&from=1800-01-01&to=1803-12-31&bucket=month
[P99]: https://www.loc.gov/resource/sn82014782/1800-05-22/ed-1/?sp=3&q=cow+pock+cow+pox
[A100]: https://usnewsmap.com/v1/aggregate?q=Erie+canal&mode=phrase&from=1825-01-01&to=1826-12-31&bucket=month
[P100]: https://www.loc.gov/resource/sn85025006/1825-01-11/ed-1/?sp=3&q=erie+canal
