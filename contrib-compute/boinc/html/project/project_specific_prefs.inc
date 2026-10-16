<?php
require_once(__DIR__."/usnm_stock_prefs.inc");

function project_specific_prefs_default() {
    return usnm_stock_prefs_default();
}

function project_specific_prefs_parse($xml) {
    return usnm_stock_prefs_parse($xml);
}

function project_specific_prefs_edit($prefs, $error=false) {
    usnm_stock_prefs_edit($prefs, $error);
    row2("Project participation",
        "Participation includes production and experimental tasks, including Qwen."
        ." Your normal CPU/GPU choices and resource limits still apply."
        ." Task diagnostics return automatically; experimental tasks may show Computation error."
        ." No separate experiment enrollment or profile update is needed.");
}

function project_specific_prefs_parse_form(&$error) {
    return usnm_stock_prefs_parse_form($error);
}

function project_specific_prefs_show($prefs, $columns=false) {
    usnm_stock_prefs_show($prefs, $columns);
}
