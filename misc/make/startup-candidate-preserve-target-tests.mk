include misc/make/startup-candidate-preserve-target-preparation-tests.mk
include misc/make/startup-candidate-preserve-target-creation-tests.mk
include misc/make/startup-candidate-preserve-target-normalization-tests.mk

.PHONY: forge-startup-usr-rollback-candidate-preserve-target-test

forge-startup-usr-rollback-candidate-preserve-target-test: \
	forge-startup-usr-rollback-candidate-preserve-target-preparation-test \
	forge-startup-usr-rollback-candidate-preserve-target-creation-test \
	forge-startup-usr-rollback-candidate-preserve-target-normalization-test
