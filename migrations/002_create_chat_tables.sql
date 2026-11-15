-- Chat tables (rooms, room_members, messages).
-- Includes encrypted-chat columns and member roles from day one:
--   - rooms.is_encrypted marks one-on-one encrypted sessions
--   - messages.encrypted_content BYTEA is NULL for regular messages
--   - messages.content is nullable (NULL for encrypted messages)
--   - room_members.role distinguishes group admins from members

CREATE TABLE IF NOT EXISTS rooms (
    id UUID PRIMARY KEY,
    name TEXT,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    is_group BOOLEAN NOT NULL DEFAULT false,
    is_encrypted BOOLEAN NOT NULL DEFAULT false
);

CREATE TABLE IF NOT EXISTS room_members (
    room_id UUID NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    role TEXT NOT NULL DEFAULT 'member',
    PRIMARY KEY (room_id, user_id),
    CONSTRAINT chk_room_member_role CHECK (role IN ('admin', 'member'))
);

CREATE TABLE IF NOT EXISTS messages (
    id UUID PRIMARY KEY,
    room_id UUID NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    sender_id UUID REFERENCES users(id) ON DELETE SET NULL,
    content TEXT,
    encrypted_content BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Messages use keyset pagination: WHERE room_id = $1
-- ORDER BY created_at DESC, id DESC. Index matches the (room_id, created_at, id)
-- access pattern used by src/chat/message.rs.
CREATE INDEX IF NOT EXISTS idx_messages_room_created
    ON messages (room_id, created_at DESC, id DESC);

-- Rooms of a user: list_rooms joins room_members on user_id (src/chat/room.rs).
CREATE INDEX IF NOT EXISTS idx_room_members_user ON room_members (user_id);

-- Room lists are sorted by created_at DESC (src/chat/room.rs).
CREATE INDEX IF NOT EXISTS idx_rooms_created_at ON rooms (created_at DESC);

-- A pending request records consent before creating a private room.
CREATE TABLE IF NOT EXISTS room_requests (
    id           UUID PRIMARY KEY,
    sender_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    receiver_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    message      TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'accepted', 'declined', 'expired', 'cancelled')),
    is_encrypted BOOLEAN NOT NULL DEFAULT false,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    responded_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_room_requests_pending_pair
    ON room_requests (sender_id, receiver_id) WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS idx_room_requests_receiver_status
    ON room_requests (receiver_id, status);

CREATE INDEX IF NOT EXISTS idx_room_requests_sender_status
    ON room_requests (sender_id, status, created_at);

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
