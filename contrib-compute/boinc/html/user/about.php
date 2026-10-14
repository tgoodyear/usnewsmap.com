<?php
// The stock nav links "About" here (html/inc/bootstrap.inc); BOINC ships no default page.
// Installed into the project tree by entrypoint.sh on every --serve start.
require_once("../inc/util.inc");

page_head(tra("About %1", PROJECT));
echo <<<HTML
<p><a href="https://usnewsmap.com">US News Map</a> searches 23 million pages of historical
American newspapers and maps every match by where and when it was printed. The pages come from
<a href="https://www.loc.gov/collections/chronicling-america/">Chronicling America</a>, the
Library of Congress and National Endowment for the Humanities collection of digitized
newspapers, 1736&ndash;1963.</p>

<h3>What your computer does</h3>
<p>Each task is one scanned newspaper page. Your computer runs Tesseract 5.5.0 on it, with
the same models and language routing the project's own OCR workers use, and sends back the
recognized text with its layout (ALTO). The project checks each result before it goes into
the search index.</p>

<h3>Requirements</h3>
<ul>
<li>Linux on x86-64, with BOINC client 9.0 or later.</li>
<li>Docker, and permission for BOINC to run Docker containers. Tasks run inside a pinned
container image. Hosts without that permission, including restricted or unprivileged
containers, can't take part yet.</li>
</ul>

<h3>What you receive</h3>
<p>Page images and the settings needed to process them. Tasks never carry passwords, keys or
access to the project's storage.</p>

<h3>Status</h3>
<p>This is a pilot for a small group of friends. Work is sent in batches of 1 to 100 pages.</p>

<p>Source code and design notes:
<a href="https://github.com/tgoodyear/usnewsmap.com/blob/main/docs/boinc.md">docs/boinc.md</a>.</p>
HTML;
page_tail();
