CREATE TABLE stored_files (
    content_hash CHAR(64) PRIMARY KEY,
    byte_size BIGINT NOT NULL CHECK (byte_size > 0),
    reference_count BIGINT NOT NULL DEFAULT 0 CHECK (reference_count >= 0)
);

CREATE TABLE file_attachments (
    message_id UUID PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    content_hash CHAR(64) NOT NULL REFERENCES stored_files(content_hash),
    original_name TEXT NOT NULL,
    media_type TEXT NOT NULL,
    byte_size BIGINT NOT NULL CHECK (byte_size > 0),
    encrypted BOOLEAN NOT NULL,
    encrypted_metadata TEXT
);

CREATE INDEX file_attachments_content_hash_index ON file_attachments (content_hash);

CREATE INDEX stored_files_unreferenced_index
    ON stored_files (content_hash) WHERE reference_count = 0;

CREATE FUNCTION update_stored_file_reference_count()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE stored_files
        SET reference_count = reference_count + 1
        WHERE content_hash = NEW.content_hash;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'Stored file does not exist for hash %', NEW.content_hash;
        END IF;
        RETURN NEW;
    END IF;

    UPDATE stored_files
    SET reference_count = reference_count - 1
    WHERE content_hash = OLD.content_hash AND reference_count > 0;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'Stored file reference count is invalid for hash %', OLD.content_hash;
    END IF;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER file_attachments_reference_count_trigger
AFTER INSERT OR DELETE ON file_attachments
FOR EACH ROW EXECUTE FUNCTION update_stored_file_reference_count();
