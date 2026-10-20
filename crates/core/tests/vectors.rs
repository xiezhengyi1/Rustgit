//! Format test vectors. Changing any expected value is a repository-format break.

use rustgit_core::objects::*;
use rustgit_store::{hash_object, ObjectId, ObjectType};

fn id(n: u8) -> ObjectId {
    ObjectId([n; 32])
}

fn sample() -> (Tree, Manifest, Commit, ObjectId) {
    let chunk = hash_object(ObjectType::Chunk, b"hello\n");
    let tree = Tree {
        entries: vec![
            TreeEntry {
                name: "a.txt".into(),
                kind: EntryKind::File,
                exec: false,
                id: chunk,
            },
            TreeEntry {
                name: "run.sh".into(),
                kind: EntryKind::File,
                exec: true,
                id: chunk,
            },
            TreeEntry {
                name: "src".into(),
                kind: EntryKind::Dir,
                exec: false,
                id: id(7),
            },
        ],
    };
    let manifest = Manifest {
        total_len: 2_000_000,
        chunks: vec![id(1), id(2)],
    };
    let commit = Commit {
        parents: vec![id(3)],
        tree: tree.id(),
        author_name: "Ann".into(),
        author_email: "ann@example.com".into(),
        timestamp: 1_700_000_000,
        tz_offset_min: -300,
        message: "first\n".into(),
    };
    (tree, manifest, commit, chunk)
}

#[test]
fn hash_vectors_and_domain_separation() {
    let (_, _, _, chunk) = sample();
    assert_eq!(
        chunk.to_hex(),
        "1cbf02a266190f8f3c946ac2b935d4e50188de69108278134d53a6a7e30db672"
    );
    assert_eq!(
        hash_object(ObjectType::Chunk, b"").to_hex(),
        "0c536a9db90a10ac9c5cd9953813bd5a417619a001f6966d5bbab7bcc193012a"
    );
    // The same bytes under another object type hash differently.
    assert_eq!(
        hash_object(ObjectType::Tree, b"hello\n").to_hex(),
        "e59c28192d2afd26190f38c72fe2b222dd69040a7a939cdd61bdb0cbcfa294a6"
    );
}

#[test]
fn canonical_encodings_and_ids() {
    let (tree, manifest, commit, _) = sample();
    assert_eq!(
        hex(&tree.encode()),
        "010305612e74787401001cbf02a266190f8f3c946ac2b935d4e50188de69108278134d53a6a7e30db6720672756e2e736801011cbf02a266190f8f3c946ac2b935d4e50188de69108278134d53a6a7e30db6720373726303000707070707070707070707070707070707070707070707070707070707070707"
    );
    assert_eq!(
        tree.id().to_hex(),
        "0c24414d256d2dade9d6354d0d4f21f62fcd4133c35869bfd5b4f632693e8e11"
    );
    assert_eq!(
        hex(&manifest.encode()),
        "0180897a0201010101010101010101010101010101010101010101010101010101010101010202020202020202020202020202020202020202020202020202020202020202"
    );
    assert_eq!(
        manifest.id().to_hex(),
        "328b4771932812ebddad128765f4519d7de86dec9af56a1f70b7d3f723b8506d"
    );
    assert_eq!(
        hex(&commit.encode()),
        "010103030303030303030303030303030303030303030303030303030303030303030c24414d256d2dade9d6354d0d4f21f62fcd4133c35869bfd5b4f632693e8e1103416e6e0f616e6e406578616d706c652e636f6d80c49fd50cd7040666697273740a"
    );
    assert_eq!(
        commit.id().to_hex(),
        "419a95d147151122fbb4672ba85614ac4dea4f59ed164f500a9eae468e47a966"
    );
    // Decoding the canonical bytes gives the same values back.
    assert_eq!(Tree::decode(&tree.encode()).unwrap(), tree);
    assert_eq!(Commit::decode(&commit.encode()).unwrap(), commit);
}

#[test]
fn non_canonical_encodings_are_rejected() {
    let (tree, _, commit, _) = sample();
    // Non-minimal varint for the entry count (0x83 0x00 instead of 0x03).
    let mut b = tree.encode();
    b.splice(1..2, [0x83, 0x00]);
    assert!(Tree::decode(&b).is_err());
    // Trailing byte after a commit.
    let mut c = commit.encode();
    c.push(0);
    assert!(Commit::decode(&c).is_err());
    // Unknown version.
    let mut t = tree.encode();
    t[0] = 2;
    assert!(Tree::decode(&t).is_err());
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
