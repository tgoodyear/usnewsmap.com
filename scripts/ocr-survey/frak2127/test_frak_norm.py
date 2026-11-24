from frak_norm import normalise


def test_rule():
    assert normalise("Städtiſches  Haus") == "Stadtisches Haus".replace("Stadt", "Städt")
    assert normalise("„Wort“ ‚x‘ «y» 'z'") == '"Wort" "x" "y" "z"'
    assert normalise("a—b⸗ c¬ d= e–f") == "a-b- c- d- e-f"
    assert normalise("ꝛä\n\t x ") == "rä x"
