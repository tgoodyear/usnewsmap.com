<?php
require_once(__DIR__."/usnm_stock_prefs.inc");

const QWEN_CANARY_LABEL = "Experimental native NVIDIA Qwen canary (unvalidated; opt in)";

function project_specific_prefs_default() {
    return usnm_stock_prefs_default()."<qwen_native_cuda_canary>0</qwen_native_cuda_canary>\n";
}

function project_specific_prefs_parse($xml) {
    $prefs = usnm_stock_prefs_parse($xml);
    $parsed = simplexml_load_string("<foo>$xml</foo>");
    $prefs->qwen_native_cuda_canary = $parsed !== false && (string)$parsed->qwen_native_cuda_canary === "1" ? 1 : 0;
    return $prefs;
}

function project_specific_prefs_edit($prefs, $error=false) {
    usnm_stock_prefs_edit($prefs, $error);
    $checked = !empty($prefs->qwen_native_cuda_canary) ? " checked" : "";
    row2('<label for="qwen_native_cuda_canary">'.QWEN_CANARY_LABEL.'</label>',
        '<input type="checkbox" id="qwen_native_cuda_canary" name="qwen_native_cuda_canary" value="1"'.$checked.'>'
        .' Native CUDA 12.4; BOINC 8.2.11+; 12/18 GiB free disk; 3/7 GB GPU memory.'
        .' May fail. No WSL, Podman or CUDA toolkit required. Opt out here at any time.');
}

function project_specific_prefs_parse_form(&$error) {
    $xml = usnm_stock_prefs_parse_form($error);
    $enabled = isset($_GET["qwen_native_cuda_canary"]) && $_GET["qwen_native_cuda_canary"] === "1";
    return $xml."<qwen_native_cuda_canary>".($enabled ? "1" : "0")."</qwen_native_cuda_canary>\n";
}

function project_specific_prefs_show($prefs, $columns=false) {
    usnm_stock_prefs_show($prefs, $columns);
    if ($columns) {
        row_defs(QWEN_CANARY_LABEL, "qwen_native_cuda_canary", "", "yesno", $prefs);
    } else {
        row2(QWEN_CANARY_LABEL, !empty($prefs->qwen_native_cuda_canary) ? "yes" : "no");
    }
}
