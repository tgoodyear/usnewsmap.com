<?php
// The stock nav links "About" here (html/inc/bootstrap.inc); BOINC ships no default page.
// Installed into the project tree by entrypoint.sh on every --serve start.
require_once("../inc/util.inc");

page_head(tra("About %1", PROJECT));
echo <<<HTML
<p><strong>Help make historical newspapers discoverable, not just viewable.</strong>
usnewsmap OCR uses volunteers' computers to turn historical newspaper scans into
searchable text and measure transcription quality.
<a href="https://usnewsmap.com">US News Map</a> lets readers search these newspapers
and explore where and when matches were printed.</p>

<h3>Why it matters</h3>
<p>A scan preserves a page's appearance, but search depends on optical character
recognition (OCR). Faded ink, damaged paper, unusual typefaces and complex columns
can hide names, places and passages from search. Better text can help genealogists
find family names, historians trace stories across towns, and students explore
how language and public discussion changed over time. Our newspaper scans come
from <a href="https://www.loc.gov/collections/chronicling-america/">Chronicling
America</a>, the Library of Congress and National Endowment for the Humanities
newspaper collection.</p>

<h3>What your computer contributes</h3>
<p>BOINC downloads tasks, runs the project's applications and returns results and
task diagnostics automatically. Tasks transcribe pages or image crops, or measure
existing OCR quality. Some experiments test newer methods, including Qwen:
fluent-looking text is not proof of a faithful transcription. Outputs undergo
project checks; not every task changes the public search index.</p>

<h3>Your computer, your choice</h3>
<p>Processing uses electricity, produces heat and may increase fan noise.
Applications and models need disk space and download bandwidth; model downloads
can be several gigabytes. Volunteer only on a computer you own or have permission
to use. BOINC lets you limit resources, run only while idle, pause processing, or
choose No new tasks while existing tasks finish. Local computing preferences
can affect other BOINC projects too.</p>

<h3>Getting started</h3>
<p>This is a limited pilot. Available work depends on your platform, resources
and project preferences; creating an account does not guarantee tasks.
Normal project participation includes production and experimental tasks, including
Qwen, using the same BOINC account and client. There is no experiment-specific
flag or repeated profile update. Your normal CPU/GPU choices and resource limits
still apply; operators phase experimental campaigns server-side. Experimental
tasks may fail with a normal Computation error; diagnostics return automatically,
without a manual log upload.
You can install BOINC now and register or attach later.</p>
<p>Read the
<a href="https://github.com/tgoodyear/usnewsmap.com/blob/main/docs/boinc-volunteer.md">volunteer
guide</a> for Windows, macOS and Linux setup, resource requirements and privacy
details. The <a href="apps.php">applications page</a> lists published applications
and platforms; publication alone does not establish end-to-end acceptance.</p>

<p>Source code and design notes:
<a href="https://github.com/tgoodyear/usnewsmap.com/blob/main/docs/boinc/README.md">docs/boinc/README.md</a>.</p>
HTML;
page_tail();
