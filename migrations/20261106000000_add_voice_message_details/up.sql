-- Voice messages keep the length and loudness measured while recording, so every device draws the same
-- waveform without decoding the audio, which some browsers cannot do for every recording format.
ALTER TABLE conversation_message_attachments
    ADD COLUMN voice_duration_ms INTEGER,
    ADD COLUMN voice_waveform BYTEA,
    ADD CONSTRAINT conversation_message_attachments_voice_duration
        CHECK (voice_duration_ms IS NULL OR voice_duration_ms BETWEEN 1 AND 3600000),
    ADD CONSTRAINT conversation_message_attachments_voice_waveform
        CHECK (voice_waveform IS NULL OR octet_length(voice_waveform) BETWEEN 1 AND 64);
