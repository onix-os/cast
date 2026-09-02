package gomessage

const Prefix = "cast go module fixture: vendored dependency v0.1.0"

func Message(subject string) string {
	return Prefix + ": " + subject
}
