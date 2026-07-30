! Fortran 2018 RANDOM_INIT must restart a repeatable sequence and accept
! both keyword orderings and nondefault scalar LOGICAL kinds.
!
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_random_init(
program ar53_random_init
  implicit none

  logical(1) :: repeatable
  logical(8) :: image_distinct
  real(8) :: first(3), advanced(3), repeated(3)
  real(8) :: distinct_first, distinct_repeated, nonrepeatable

  repeatable = .true.
  image_distinct = .false.
  call random_init(image_distinct=image_distinct, repeatable=repeatable)
  call random_number(first)
  call random_number(advanced)

  call random_init(repeatable, image_distinct)
  call random_number(repeated)
  if (any(first /= repeated)) error stop 1
  if (all(first == advanced)) error stop 2

  call random_init(.true., .true.)
  call random_number(distinct_first)
  call random_init(image_distinct=.true., repeatable=.true.)
  call random_number(distinct_repeated)
  if (distinct_first /= distinct_repeated) error stop 3

  call random_init(.false., .false.)
  call random_number(nonrepeatable)
  if (nonrepeatable < 0.0_8 .or. nonrepeatable >= 1.0_8) error stop 4

  print '(a)', 'ok'
end program ar53_random_init
