<?php

function project_footer($show_return, $show_date, $prefix) {
    echo '<footer class="text-center" style="padding: 1.5rem 0;">';
    echo '<a href="https://boinc.berkeley.edu/">Powered by BOINC</a>';
    if ($show_date) {
        echo '<br><small>'.tra('Generated').' '.time_str(time()).'</small>';
    }
    echo '</footer>';
}
