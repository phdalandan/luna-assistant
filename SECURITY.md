# Security policy

Luna controls physical devices in people's homes, so security reports are taken seriously.

## Reporting a vulnerability

Please do not open a public issue. Report privately through [GitHub security advisories](../../security/advisories/new).

Include the affected version, platform, steps to reproduce, and the impact. You will get an acknowledgement within 7 days.

## Scope

Relevant reports include:

- Actions executed without required validation or confirmation
- The AI model reaching services, entities, or system commands outside the allowed tools
- Credential exposure in logs, storage, prompts, or the interface
- Audio or transcripts leaving the device
- Vulnerabilities in the build and release workflow

## Design commitments

- Home Assistant tokens are stored in the operating system credential store and never logged or sent to the model.
- All speech and language processing runs locally.
- Recordings are not stored by default.
- Security-sensitive actions such as unlocking doors require confirmation.
