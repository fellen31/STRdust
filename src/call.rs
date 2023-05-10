use bio::io::bed;
use log::{error, info};
use rayon::prelude::*;
use regex::Regex;
use rust_htslib::faidx;
use rust_htslib::{bam, bam::Read};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use minimap2::*;

pub fn genotype_repeats(
    bamp: PathBuf,
    fasta: PathBuf,
    region: Option<String>,
    region_file: Option<PathBuf>,
    minlen: usize,
    support: usize,
    threads: usize,
    somatic: bool,
) {
    if !bamp.is_file() {
        error!(
            "ERROR: path to bam file {} is not valid!\n\n",
            bamp.display()
        );
        panic!();
    };
    match (region, region_file) {
        (Some(_region), Some(_region_file)) => {
            error!("ERROR: Specify either a region (-r) or region_file (-R), not both!\n\n");
            panic!();
        }
        (None, None) => {
            error!("ERROR: Specify one of region (-r) or region_file (-R)!\n\n");
            panic!();
        }
        (Some(region), None) => {
            let (chrom, start, end) = crate::utils::process_region(region)
                .expect("Error when processing the region string");
            let bamf = bamp.into_os_string().into_string().unwrap();
            let fastaf = fasta.into_os_string().into_string().unwrap();
            match genotype_repeat(&bamf, &fastaf, chrom, start, end, minlen, support) {
                Ok(output) => println!("{output}"),
                Err(chrom) => error!("Contig {chrom} not found in bam file"),
            };
        }
        (None, Some(region_file)) => {
            if threads > 1 {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                // TODO: check if bed file is okay
                let mut reader =
                    bed::Reader::from_file(region_file.into_os_string().into_string().unwrap())
                        .unwrap();
                let bamf = bamp.into_os_string().into_string().unwrap();
                let fastaf = fasta.into_os_string().into_string().unwrap();
                // chrom_reported and genotypes are vectors that are used by multiple threads to add findings, therefore as a Mutex
                // chrom_reported contains those chromosomes for which an error (absence in the bam) was already reported
                // to avoid reporting the same error multiple times
                let chrom_reported = Mutex::new(Vec::new());
                // genotypes contains the output of the genotyping, a struct instance
                let genotypes = Mutex::new(Vec::new());
                // par_bridge does not guarantee that results are returned in order
                reader.records().par_bridge().for_each(|record| {
                    let rec = record.expect("Error reading bed record.");
                    match genotype_repeat(
                        &bamf,
                        &fastaf,
                        rec.chrom().to_string(),
                        rec.start().try_into().unwrap(),
                        rec.end().try_into().unwrap(),
                        minlen,
                        support,
                        somatic,
                    ) {
                        Ok(output) => {
                            let mut geno = genotypes.lock().unwrap();
                            geno.push(output);
                        }
                        Err(chrom) => {
                            // For now the Err is only used for when a chromosome from the bed file does not appear in the bam file
                            // this error is reported once per chromosome
                            let mut chroms_reported = chrom_reported.lock().unwrap();
                            if !chroms_reported.contains(&chrom) {
                                error!("Contig {chrom} not found in bam file");
                                chroms_reported.push(chrom);
                            }
                        }
                    };
                });
                let mut genotypes_vec = genotypes.lock().unwrap();
                // The final output is sorted by chrom, start and end
                genotypes_vec.sort_unstable();
                for g in &mut *genotypes_vec {
                    println!("{g}");
                }
            } else {
                // When running single threaded things become easier and the tool will require less memory
                // Output is returned in the same order as the bed, and therefore not sorted before writing to stdout
                // TODO: check if bed file is okay
                let mut reader =
                    bed::Reader::from_file(region_file.into_os_string().into_string().unwrap())
                        .unwrap();
                let bamf = bamp.into_os_string().into_string().unwrap();
                let fastaf = fasta.into_os_string().into_string().unwrap();
                // chrom_reported contains those chromosomes for which an error (absence in the bam) was already reported
                // to avoid reporting the same error multiple times
                let mut chrom_reported = Vec::new();
                // genotypes contains the output of the genotyping, a struct instance
                for record in reader.records() {
                    let rec = record.expect("Error reading bed record.");
                    match genotype_repeat(
                        &bamf,
                        &fastaf,
                        rec.chrom().to_string(),
                        rec.start().try_into().unwrap(),
                        rec.end().try_into().unwrap(),
                        minlen,
                        support,
                        somatic,
                    ) {
                        Ok(output) => {
                            println!("{output}");
                        }
                        Err(chrom) => {
                            // For now the Err is only used for when a chromosome from the bed file does not appear in the bam file
                            // this error is reported once per chromosome
                            if !chrom_reported.contains(&chrom) {
                                error!("Contig {chrom} not found in bam file");
                                chrom_reported.push(chrom);
                            }
                        }
                    };
                }
            }
        }
    }
}

/// This function genotypes a particular repeat defined by chrom, start and end in the specified bam file
/// All indel cigar operations longer than minlen are considered
/// The bam file is expected to be phased using the HP tag
fn genotype_repeat(
    bamf: &String,
    fasta: &String,
    chrom: String,
    start: u32,
    end: u32,
    minlen: usize,
    support: usize,
    somatic: bool,
) -> Result<String, String> {
    let newref = make_repeat_compressed_sequence(fasta, &chrom, start, end);
    let seqs = get_overlapping_reads(bamf, chrom.clone(), start, end).expect("Could not get reads");
    let aligner = minimap2::Aligner::builder()
        .map_ont()
        .with_cigar()
        .with_seq(&newref)
        .expect("Unable to build index");
    // align the reads to the new repeat-compressed reference
    // a regular expression that splits the cs tag ':32*nt*na:10-gga:5+aaa:10' into (':32', '*nt', '*na', ':10', '-gga', ':5', '+aaa', ':10')
    let mut consenses = vec![];
    let mut all_insertions = vec![]; // only used with `somatic`

    for phase in [1, 2] {
        let mut insertions = vec![];
        let seq = seqs.get(&phase).unwrap();
        for s in seq {
            let mapping = aligner.map(s.as_slice(), true, false, None, None).unwrap_or_else(|_| panic!("Unable to align read with seq {s:?} to repeat-compressed reference for {chrom}:{start}-{end}", s=s.to_ascii_uppercase(), chrom=chrom, start=start, end=end));
            for read in mapping {
                if let Some(s) = parse_cs(read, minlen) {
                    insertions.push(s)
                }
            }
        }
        consenses.push(crate::consensus::consensus(&insertions, support));
        if somatic {
            // store all inserted sequences for identifying somatic variation
            all_insertions.push(insertions.join(","));
    }
    let (length1, seq1) = match consenses[0] {
        Some(ref s) => (s.len().to_string(), s.clone()),
        None => (".".to_string(), ".".to_string()),
    };
    let (length2, seq2) = match consenses[1] {
        Some(ref s) => (s.len().to_string(), s.clone()),
        None => (".".to_string(), ".".to_string()),
    };

    let somatic_info_field = if somatic {
        format!(";SEQS={}", all_insertions.join("|"))
    } else {
        "".to_string()
    };

    Ok(format!(
        "{chrom}\t{start}\t.\t{ref}\t{alt1},{alt2}\t.\t.\t\
    ))
}

fn make_repeat_compressed_sequence(
    fasta: &String,
    chrom: &String,
    start: u32,
    end: u32,
) -> Vec<u8> {
    let fas = faidx::Reader::from_path(fasta).expect("Failed to read fasta");
    // TODO make sure we cannot try to read past the end of chromosomes
    let fas_left = fas
        .fetch_seq(chrom, (start - 10002) as usize, start as usize - 2)
        .expect("Failed to extract fas_left sequence from fasta");
    let fas_right = fas
        .fetch_seq(chrom, end as usize, (end + 9998) as usize)
        .expect("Failed to extract fas_right sequence from fasta");
    [fas_left, fas_right].concat()
}

fn get_overlapping_reads(
    bamf: &String,
    chrom: String,
    start: u32,
    end: u32,
) -> Option<HashMap<u8, Vec<Vec<u8>>>> {
    let mut bam = bam::IndexedReader::from_path(bamf).expect("Error opening BAM");
    info!("Checks passed, genotyping repeat");
    if let Some(tid) = bam.header().tid(chrom.as_bytes()) {
        bam.fetch((tid, start, end))
            .expect("Failure to extract reads from bam.");
        // Per haplotype the read sequences are kept in a dictionary
        let mut seqs = HashMap::from([(1, Vec::new()), (2, Vec::new())]);

        // extract sequences spanning the repeat locus
        for r in bam.rc_records() {
            let r = r.expect("Error reading BAM file in region {chrom}:{start}-{end}.");
            let phase = crate::utils::get_phase(&r);
            if phase > 0 {
                let seq = r.seq().as_bytes();
                seqs.get_mut(&phase).unwrap().push(seq);
            }
        }
        return Some(seqs);
    }
    None
}

fn parse_cs(read: Mapping, minlen: usize) -> Option<String> {
    let mut ref_pos = read.target_start;
    let alignment = read.alignment.expect("Unable to access alignment");
    let cs = alignment.cs.expect("Unable to get the cs field");

    // split the cs tag using the regular expression
    let re = Regex::new(r"(:\d+)|(\*\w+)|(\+\w+)|(-\w+)").unwrap();

    let mut insertions = Vec::new();

    for cap in re.captures_iter(&cs) {
        let op = &cap[0].chars().next().unwrap();
        match op {
            ':' => {
                // match consumes the reference (and the read)
                ref_pos += cap[0][1..]
                    .parse::<i32>()
                    .expect("Unable to parse length from CS':' operation");
            }
            '*' => {
                // mismatch consumes the reference (and the read)
                // the cs tag is of the form *na, where n is the ref nucleotide and a the read nucleotide
                // therefore the number of nucleotides to advance is half the length of sequence from this cs operation
                ref_pos += cap[0][1..].len() as i32 / 2;
            }
            '-' => {
                // deletion consumes the reference
                // the cs tag is of the form -gga, where gga is the deleted sequence
                // therefore the number of nucleotides to advance is the length of sequence from this cs operation
                ref_pos += cap[0][1..].len() as i32;
            }
            '+' => {
                // insertion consumes the read
                // we only care about insertions that are within +/-10 bases of the repeat locus that was excised from the reference
                // this is the ref_pos
                // the cs tag is of the form +aaa, where aaa is the inserted sequence
                // if the insertion is longer than the minimum length, it is added to the list of insertions
                if cap[0][1..].len() > minlen && (9990..=10010).contains(&ref_pos) {
                    insertions.push(cap[0][1..].to_string());
                }
            }
            _ => {
                // error
                panic!("Unexpected operation in cs tag: {op}");
            }
        }
    }
    if !insertions.is_empty() {
        Some(insertions.join(""))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_make_repeat_compressed_sequence() {
        let fasta = String::from("test_data/chr7.fa.gz");
        let chrom = String::from("chr7");
        let start = 154654404;
        let end = 154654432;
        let seq = make_repeat_compressed_sequence(&fasta, &chrom, start, end);
        // println!("{:?}", std::str::from_utf8(&seq[9990..10010]));
        // println!("{:?}", std::str::from_utf8(&seq));
        assert_eq!(seq.len(), 20000);
    }

    #[test]
    fn test_get_overlapping_reads() {
        let bam = String::from("test_data/small-test-phased.bam");
        let chrom = String::from("chr7");
        let start = 154654404;
        let end = 154654432;
        let _reads = get_overlapping_reads(&bam, chrom, start, end);
    }

    #[test]
    fn test_parse_cs() {
        let bam = String::from("test_data/small-test-phased.bam");
        let fasta = String::from("test_data/chr7.fa.gz");
        let chrom = String::from("chr7");
        let start = 154654404;
        let end = 154654432;
        let minlen = 5;
        let _support = 1;
        let newref = make_repeat_compressed_sequence(&fasta, &chrom, start, end);
        let binding = get_overlapping_reads(&bam, chrom.clone(), start, end).unwrap();
        let read = binding
            .get(&1)
            .expect("Could not find phase 1")
            .iter()
            .next()
            .expect("No reads found");
        let aligner = minimap2::Aligner::builder()
            .map_ont()
            .with_cigar()
            .with_seq(&newref)
            .expect("Unable to build index");
        let mapping = aligner.map(read.as_slice(), true, false, None, None).unwrap_or_else(|_| panic!("Unable to align read with seq {read:?} to repeat-compressed reference for {chrom}:{start}-{end}", read=read.to_ascii_uppercase(), chrom=chrom, start=start, end=end));

        let _insertion = parse_cs(
            mapping
                .first()
                .expect("Could not get first mapping")
                .clone(),
            minlen,
        );
    }

    #[test]
    fn test_genotype_repeat() {
        let bam = String::from("test_data/small-test-phased.bam");
        let fasta = String::from("test_data/chr7.fa.gz");
        let chrom = String::from("chr7");
        let start = 154654404;
        let end = 154654432;
        let minlen = 5;
        let support = 1;
        let genotype = genotype_repeat(&bam, &fasta, chrom, start, end, minlen, support);
        println!("{}", genotype.expect("Unable to genotype repeat"));
    }
}
