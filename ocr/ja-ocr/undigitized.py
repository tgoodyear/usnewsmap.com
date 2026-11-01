"""Target pages with no scan to read (#1415): the placeholders LoC lists as undigitized.

Each issue's metadata on loc.gov keeps a slot for these pages, marked `digitized: false`, with no
image, so there is nothing to fetch or OCR. They stay in the target list and out of every "read"
count. Progress reports them on their own (`undigitized` in status/ocr-ja.json), and the percent
is of the digitized pages.

The list comes from the availability audit of 27 October 2026, the GitHub release named below.
`classification.json` there holds all 43 pages left at the time, and `issue-metadata.tar.gz` the
LoC metadata each entry was read from; each page's hash below is its issue's metadata in that
archive. A page leaves this list only when a new audit finds an image for it. `test_undigitized.py`
checks the list's shape.
"""

RELEASE = "data-ja-availability-20261027"
CLASSIFICATION_SHA256 = "8c3940979c348bdae30df8360e4a99b22934bd2c7c146f9de3adce344300ea7e"

# doc_id, sha256 of the issue's metadata in the release.
PAGES: tuple[tuple[str, str], ...] = (
    ("sn83025333_1943-01-20_ed-1_seq-5",
     "c5e6d60b7d1a2a473b9ad654a15968b0fc81dfd678fe6b9b109d159428783b4f"),
    ("sn83025333_1943-01-30_ed-1_seq-7",
     "202ce37302fcc72f3ce04e38cdd95cd980eaf7d5250208cef2fa0a31afefc6cb"),
    ("sn83025333_1943-03-23_ed-1_seq-4",
     "2c19dcd8e51d91a97724382fb17b8084b4920929ee4d6862ea7907d211e541a6"),
    ("sn83025333_1943-03-23_ed-1_seq-5",
     "2c19dcd8e51d91a97724382fb17b8084b4920929ee4d6862ea7907d211e541a6"),
    ("sn83025333_1943-09-10_ed-1_seq-4",
     "62cb5cfbff4b036b732cc71c16becd2c0800dd13345f8ec876b6252343ff882a"),
    ("sn83025333_1943-09-10_ed-1_seq-5",
     "62cb5cfbff4b036b732cc71c16becd2c0800dd13345f8ec876b6252343ff882a"),
    ("sn83025333_1943-09-10_ed-1_seq-6",
     "62cb5cfbff4b036b732cc71c16becd2c0800dd13345f8ec876b6252343ff882a"),
    ("sn83025333_1943-12-09_ed-1_seq-3",
     "c3c158d57a3a8321c857cf9f888336637f9c815edc618a4de9c20c02ba620eb4"),
    ("sn83025333_1944-01-29_ed-1_seq-2",
     "57f0e669db3cd6d1245ed8545037d3fe3a39d76bca601369b790693e0728a798"),
    ("sn83025353_1943-02-04_ed-1_seq-8",
     "861d5a862509ac9bbeb9a8acc60704b511f0bc51e9815dd300df44622ac27aa9"),
    ("sn83025353_1944-06-17_ed-1_seq-9",
     "83f8fba76ef30ad4d52cf6f7e5e4870055fec4e025942ac233960083d85a2e62"),
    ("sn83025353_1944-07-06_ed-1_seq-8",
     "8cf0ce345d0d7e82e09443c211c993361a33e65e74a9df5e11996f90b01722b3"),
    ("sn83025522_1944-06-21_ed-1_seq-7",
     "1b12aa86c88ce4a1fca1686e32e0b607b41b382f89fbb952a74ca6bdd1618e8b"),
    ("sn83025522_1944-11-08_ed-1_seq-6",
     "4a17846d578fe974585ee266a0366cbbe593119621aa8d9c9412aae7286310d2"),
    ("sn84025150_1943-08-21_ed-1_seq-6",
     "1b83065a5bc7f177b0dc7829c62e4ab9c2bc89b6f99c7bf5f88640e43dbc6f05"),
    ("sn84025948_1943-10-13_ed-1_seq-3",
     "fb9a66b3b546f902da36d1f209fb9db5a6e9a2cbef24af5f227be2070654ac23"),
    ("sn84025954_1943-01-04_ed-1_seq-4",
     "c0872b3a5cfe36c26aa36545821495c079cabfff579d71bde745c291f33745bd"),
    ("sn84025954_1943-08-05_ed-1_seq-2",
     "0ca6eeaaf14a09ea9de005fbb53d763559691dad378787fcfa2a2b07554fc111"),
    ("sn84025954_1943-08-05_ed-1_seq-3",
     "0ca6eeaaf14a09ea9de005fbb53d763559691dad378787fcfa2a2b07554fc111"),
    ("sn84025954_1943-08-28_ed-1_seq-3",
     "27f02dc2a915c587137cea4fca6395051932a19da52c775b695d904e113a12e4"),
    ("sn85040043_1942-10-01_ed-1_seq-3",
     "b6ab8f767ce9588840bb57245659a5e2d086dc90e90b9f96295f92b44f5a212f"),
    ("sn87062142_1943-04-10_ed-1_seq-3",
     "64a2b7a4fa19cdc1b48b283a8565e6e31d4cfe7d0b6625e81d21ed1ab7dfb512"),
    ("sn87062142_1943-04-10_ed-1_seq-4",
     "64a2b7a4fa19cdc1b48b283a8565e6e31d4cfe7d0b6625e81d21ed1ab7dfb512"),
    ("sn94050512_1943-09-28_ed-1_seq-2",
     "f56a21e91351aaac40175cf8e9f39e1b507b124f0f7f761acb404c08ece0122e"),
)

DOC_IDS = frozenset(doc_id for doc_id, _ in PAGES)
