package eval

// Standard is the set of variants worth comparing today: the baseline and the
// recipient features. own are the addresses mail is always expected on. When a
// candidate is adopted into the tokenizer, drop it here: it is then baseline.
func Standard(own []string) []Variant {
	return []Variant{
		Baseline(),
		Recipients(RecipientOptions{Status: true, Own: own}),
		Recipients(RecipientOptions{PerAddress: true, Own: own}),
		Recipients(RecipientOptions{Status: true, PerAddress: true, Own: own}),
	}
}
