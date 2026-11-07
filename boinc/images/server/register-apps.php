#!/usr/bin/env php
<?php
// The register-apps bridge step, run in the project directory: BOINC's bin/xadd, then the
// project.xml <weight> for each app that has no app version yet.
//
// At the pinned BOINC commit (22bbebe, tools/xadd) xadd inserts an app with only its name and
// user_friendly_name, so the database gives it weight 1. An app added at weight 0
// (usnm_gold_native, docs/boinc-native-ocr.md) would then shrink every other app's feeder query
// budget. This sets the declared weight on each app that has no app version yet (one xadd just
// added, or one an earlier run added and then failed on), reads it back, and fails if it doesn't
// hold, all before the feeder restarts with the new app.
//
// It never changes an app that has an app version. A weight change there moves live slot shares
// and needs a feeder-only boundary (boinc/docs/app-release.md). Such an app whose weight
// differs from project.xml is reported, and left as it is.

function fail($message) {
    fwrite(STDERR, "register-apps: $message\n");
    exit(1);
}

chdir("html/inc");
require_once("boinc_db.inc");
chdir("../..");

$x = simplexml_load_file("project.xml");
if ($x === false) {
    fail("can't read project.xml");
}

$declared = [];
foreach ($x->app as $app) {
    $name = (string)$app->name;
    if (!preg_match('/^[a-z][a-z0-9_]{0,63}$/', $name)) {
        fail("bad app name in project.xml: $name");
    }
    $weight = trim((string)$app->weight);
    if (!preg_match('/^[0-9]{1,4}$/', $weight)) {
        fail("app $name needs a <weight> of 0 to 9999 in project.xml");
    }
    $declared[$name] = (int)$weight;
}

passthru("bin/xadd", $status);
if ($status !== 0) {
    fail("bin/xadd exited $status");
}

foreach ($declared as $name => $weight) {
    $row = BoincApp::lookup("name='$name'");
    if (!$row) {
        fail("app $name isn't in the database after xadd");
    }
    if ((float)$row->weight == $weight) {
        continue;
    }
    $id = (int)$row->id;
    if (BoincAppVersion::lookup("appid=$id")) {
        echo "app $name has weight $row->weight in the database and $weight in project.xml; not changed, it has app versions\n";
        continue;
    }
    $row->update("weight=$weight");
    $row = BoincApp::lookup("name='$name'");
    if (!$row || (float)$row->weight != $weight) {
        fail("app $name has no app version, but its weight isn't $weight");
    }
    echo "set weight of app $name, which has no app version, to $weight\n";
}
