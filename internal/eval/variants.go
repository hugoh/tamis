package eval

// Standard is the set of variants worth comparing today: the baseline, and
// ablations that show what its parts contribute. own are the addresses mail
// is always expected on. A candidate feature goes here until it is adopted
// into the tokenizer, when it becomes the baseline.
func Standard(own []string) []Variant {
	return []Variant{
		Baseline(own),
		WithoutRecipients(),
	}
}
