! ES and EN output renormalize a mantissa that crosses its upper bound
! after rounding, while neighboring non-carry values keep their scale.
!
! FLAGS: --std=f2018
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_fmt_push_real(
! IR_CHECK: call @afs_fmt_push_real32(
program ar44_es_en_rounding_carry
  implicit none

  character(len=13) :: es_record
  character(len=14) :: wide_record

  write(es_record, '(RU,ES13.3)') 9.9991_8
  if (es_record /= '    1.000E+01') error stop 1

  write(es_record, '(RN,ES13.3)') 9.9996_8
  if (es_record /= '    1.000E+01') error stop 2

  write(es_record, '(RZ,ES13.3)') 9.9996_8
  if (es_record /= '    9.999E+00') error stop 3

  write(wide_record, '(RU,ES14.3E3)') 9.9991_8
  if (wide_record /= '    1.000E+001') error stop 4

  write(es_record, '(RU,ES13.3)') 0.99991_8
  if (es_record /= '    1.000E+00') error stop 5

  write(es_record, '(RU,ES13.3)') 9.9991_4
  if (es_record /= '    1.000E+01') error stop 6

  write(wide_record, '(RU,EN14.3)') 999.9991_8
  if (wide_record /= '     1.000E+03') error stop 7

  write(wide_record, '(RN,EN14.3)') 999.9996_8
  if (wide_record /= '     1.000E+03') error stop 8

  write(wide_record, '(EN14.3)') 999.9996_8
  if (wide_record /= '     1.000E+03') error stop 9

  write(wide_record, '(RZ,EN14.3)') 999.9996_8
  if (wide_record /= '   999.999E+00') error stop 10

  write(wide_record, '(RU,EN14.3)') 0.9999991_8
  if (wide_record /= '     1.000E+00') error stop 11

  write(wide_record, '(RU,EN14.3)') 0.0009999991_8
  if (wide_record /= '     1.000E-03') error stop 12

  write(wide_record, '(RU,EN14.3)') 999999.9_8
  if (wide_record /= '     1.000E+06') error stop 13

  write(wide_record, '(RU,EN14.3)') 999.9991_4
  if (wide_record /= '     1.000E+03') error stop 14

  print '(a)', 'ok'
end program ar44_es_en_rounding_carry
