package application

import gomessage "fixtures.invalid/cast/go-message"

func Identity() string {
	return gomessage.Message("declarative userspace")
}
