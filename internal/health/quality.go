package health

// QualityProblems are the checks of how well the filter is learning and
// deciding that are not ok: the training and the decisions, not the setup
// (configuration, rules, web interface) or the daemon itself.
func (r *Report) QualityProblems() []Check {
	var problems []Check

	for _, check := range r.Checks {
		if check.Section != SectionTraining && check.Section != SectionDecisions {
			continue
		}

		if check.Status == StatusWarn || check.Status == StatusFail {
			problems = append(problems, check)
		}
	}

	return problems
}
